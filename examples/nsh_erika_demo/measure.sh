#!/usr/bin/env bash
# 量 player 示例跑起来之后的 CPU / GPU 占用。
#
#   ./measure.sh                          # 环境变量 TEST_VIDEO 就是片源
#   TEST_VIDEO=/path/to.mkv ./measure.sh
#   SECONDS_TO_RUN=20 WARMUP=8 ./measure.sh
#
# 也可以量一个**已经在跑的**进程（不改它的代码、不用重新启动）：
#
#   MEASURE_PID=$(pgrep -f nsh-app-erika-demo) ./measure.sh
#
# 这种模式下 WARMUP 只用来等一下（让你先切到播放页），不会杀进程。
#
# 输出：
#   - 进程 CPU%（100% = 占满一个核）
#   - 最费 CPU 的 5 个线程
#   - AMD GPU 的 busy%（有 /sys/class/drm/card*/device/gpu_busy_percent 才打）
#   - 出帧日志里的 tick 记录（NSH_ERIKA_TRACE=1 时每 2 秒一条）
#
# 前提：先 `cargo build`（或用 nsh run 跑过一次，二进制在 target 里）。
set -uo pipefail

SECONDS_TO_RUN=${SECONDS_TO_RUN:-15}
WARMUP=${WARMUP:-6}
CLK_TCK=$(getconf CLK_TCK)
LOG=/dev/null

if [[ -n ${MEASURE_PID:-} ]]; then
    # 量已有进程：不启动、不杀、不读它的日志。
    PID=$MEASURE_PID
    LOG=/dev/null
    if ! kill -0 "$PID" 2>/dev/null; then
        echo "进程 $PID 不在" >&2
        exit 1
    fi
    echo "采样已有进程 $PID"
    sleep "$WARMUP"
else
    # 二进制：显式给 BIN，或者问 cargo 要（它知道你的 target-dir 配在哪）
    if [[ -z ${BIN:-} ]]; then
        BIN=$(cargo build --message-format=json 2>/dev/null \
            | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -1)
    fi
    BIN=${BIN:-./target/debug/nsh-app-erika-demo}
    LOG=$(mktemp /tmp/nsh-erika-measure.XXXXXX.log)

    if [[ ! -x $BIN ]]; then
        echo "找不到可执行文件（cargo 没给出 --message-format=json 的 executable）" >&2
        exit 1
    fi

    RUST_LOG=${RUST_LOG:-nsh_erika=info} NSH_ERIKA_TRACE=${NSH_ERIKA_TRACE:-1} \
        "$BIN" >"$LOG" 2>&1 &
    PID=$!
    trap 'kill "$PID" 2>/dev/null; wait "$PID" 2>/dev/null' EXIT

    sleep "$WARMUP"
    if ! kill -0 "$PID" 2>/dev/null; then
        echo "进程在预热期间退了，看日志：$LOG" >&2
        tail -20 "$LOG" >&2
        exit 1
    fi
fi

cpu_ticks() { awk '{print $14 + $15}' "/proc/$1/stat"; }
# tid / 累计 tick / 线程名，一行一个
threads() {
    for task in /proc/"$PID"/task/*; do
        [[ -r $task/stat ]] || continue
        awk -v tid="${task##*/}" '{ print tid, $14 + $15, $2 }' "$task/stat"
    done
}
total_before=$(cpu_ticks "$PID")
threads >"$LOG.threads.before"
start=$(date +%s%N)
gpu_busy=""
for _ in $(seq 1 "$SECONDS_TO_RUN"); do
    for node in /sys/class/drm/card*/device/gpu_busy_percent; do
        [[ -r $node ]] || continue
        gpu_busy+=" $(cat "$node")"
    done
    sleep 1
done
total_after=$(cpu_ticks "$PID")
threads >"$LOG.threads.after"
end=$(date +%s%N)

elapsed=$(awk -v a="$start" -v b="$end" 'BEGIN { print (b - a) / 1e9 }')
cpu=$(awk -v a="$total_before" -v b="$total_after" -v hz="$CLK_TCK" -v t="$elapsed" \
    'BEGIN { printf "%.1f", (b - a) / hz / t * 100 }')

echo "采样 $elapsed s（预热 ${WARMUP}s 后）"
echo "进程 CPU: ${cpu}%   （100% = 一个核）"

echo
echo "最费 CPU 的线程:"
awk 'NR == FNR { before[$1] = $2; name[$1] = $3; next }
     $1 in before { printf "%d %s\n", $2 - before[$1], name[$1] }' \
    "$LOG.threads.before" "$LOG.threads.after" \
    | sort -rn | head -5 \
    | awk -v hz="$CLK_TCK" -v t="$elapsed" \
        '{ printf "  %-26s %6.1f%%\n", $2, $1 / hz / t * 100 }'

if [[ -n ${gpu_busy// /} ]]; then
    echo
    echo "GPU busy%:$(echo "$gpu_busy" | tr -s ' ' '\n' | awk 'NF { s += $1; n++; if ($1 > m) m = $1 } END { printf " avg=%.0f max=%.0f (n=%d)", s / n, m, n }')"
else
    echo
    echo "GPU busy%: 读不到（没有 amdgpu 的 gpu_busy_percent；NVIDIA/Intel 换用其它工具）"
fi

if [[ $LOG != /dev/null && -r $LOG ]]; then
    echo
    echo "出帧日志（最后 4 条 tick / 第一帧）:"
    grep -E "第一帧|tick " "$LOG" | tail -4 || echo "  (没有：可能没在播，或 NSH_ERIKA_TRACE=1 没生效)"
    echo
    echo "完整日志: $LOG"
fi
