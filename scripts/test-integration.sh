#!/usr/bin/env bash
# 集成测试总入口：CI 只调这一个脚本。
#
# 加新的集成测试：在下面 TESTS 里加一行（对应 `tests/<名字>.rs`）。脚本会自己检查两边
# 有没有漂移：
#   - 列表里的名字没有 `tests/<名字>.rs` → 直接报错（改名/删文件时立刻暴露）；
#   - `tests/*.rs` 里有文件没进列表 → 打 WARN（忘了加不会静默漏测）。
#
# 用法：
#   bash scripts/test-integration.sh                # 顺序跑，第一个失败就停
#   bash scripts/test-integration.sh --keep-going   # 全跑完再汇总（想一次看全用这个）
#   bash scripts/test-integration.sh --list         # 只打印清单 + 漂移检查
#   bash scripts/test-integration.sh --release      # 其余参数原样传给 cargo test
#
# 装了 Mesa 的软件 Vulkan（Arch：`vulkan-swrast`）时，`frame_output` 会**再跑一遍**只指向
# lavapipe 的版本：RADV 不允许 mmap dma-buf，像素级断言（不是纯黑 / 画面在动）只有软件
# Vulkan 那条过得去。找不到 lavapipe 的 ICD 就跳过这一遍，不影响结果。
#
# 每个用例的完整输出写在 `$CARGO_TARGET_DIR/integration-logs/<名字>.log`：
# 成功只打一行 + 耗时，失败打末尾 60 行。
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

# ── 唯一需要维护的地方：集成测试清单（顺序即执行顺序）──────────────────────
TESTS=(
  engine_state
  engine_open
  engine_playback
  events
  # 出帧路径（需要能导出 dma-buf 的 Vulkan 设备；没有时打 SKIP 不算失败）
  frame_output
)

# **所有**集成用例都带逃生舱：
#   · `tests/common` 用 `ErikaSession`（守卫）建引擎，它只在 escape-hatch 下导出；
#   · `frame_output` 的断言直接建在 `video::*` 上（不给 feature 会被
#     `#![cfg(feature = "escape-hatch")]` 整条编掉，跑出"0 个用例"还显示 PASS）。
FEATURES=(--features escape-hatch)

keep_going=0
list_only=0
cargo_args=()
for arg in "$@"; do
  case "$arg" in
    --keep-going) keep_going=1 ;;
    --list) list_only=1 ;;
    -h | --help) awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; exit 0 ;;
    *) cargo_args+=("$arg") ;;
  esac
done

logs="${CARGO_TARGET_DIR:-$root/target}/integration-logs"
mkdir -p "$logs"

# ── 漂移检查：清单 ↔ tests/ 目录 ─────────────────────────────────────────────
missing=()
for name in "${TESTS[@]}"; do
  [ -f "tests/$name.rs" ] || missing+=("$name")
done
if [ "${#missing[@]}" -gt 0 ]; then
  printf '错误：TESTS 里这些名字在 tests/ 下没有对应文件：%s\n' "${missing[*]}" >&2
  exit 1
fi

shopt -s nullglob
for file in tests/*.rs; do
  name=$(basename "$file" .rs)
  found=0
  for listed in "${TESTS[@]}"; do
    if [ "$listed" = "$name" ]; then
      found=1
      break
    fi
  done
  [ "$found" -eq 1 ] ||
    printf 'WARN：tests/%s 没进 scripts/test-integration.sh 的 TESTS，CI 不会跑它\n' "$name" >&2
done

if [ "$list_only" -eq 1 ]; then
  printf '集成测试（%d 个，按此顺序）：\n' "${#TESTS[@]}"
  printf '  %s\n' "${TESTS[@]}"
  exit 0
fi

# ── 逐个跑 ────────────────────────────────────────────────────────────────
now_ms() { echo $(($(date +%s%N) / 1000000)); }

results=()
failed=()
for name in "${TESTS[@]}"; do
  log="$logs/$name.log"
  printf '==> %s\n' "$name"
  started=$(now_ms)
  if cargo test --test "$name" "${FEATURES[@]}" ${cargo_args[@]+"${cargo_args[@]}"} >"$log" 2>&1; then
    elapsed=$(($(now_ms) - started))
    summary=$(grep -E '^test result:' "$log" | tail -1)
    results+=("PASS  $name  ${elapsed}ms  ${summary#test result: }")
    printf '    PASS (%d ms) %s\n' "$elapsed" "$summary"
  else
    elapsed=$(($(now_ms) - started))
    results+=("FAIL  $name  ${elapsed}ms  日志：$log")
    printf '    FAIL (%d ms)，日志末尾：\n' "$elapsed" >&2
    tail -60 "$log" >&2
    failed+=("$name")
    [ "$keep_going" -eq 1 ] || break
  fi
done

# ── 软件 Vulkan 复跑：像素级断言只有这一遍能过 ─────────────────────────────
# 用 loader 层只暴露 lavapipe（`VK_ICD_FILENAMES`），再配 `WGPU_BACKEND=vulkan` 防止
# erika 退到 GL 就收工（GL 也算"硬件"，会挡住软件 Vulkan 候选）。
run_lavapipe() {
  local icd
  icd=$(ls /usr/share/vulkan/icd.d/lvp_icd*.json 2>/dev/null | head -1)
  if [ -z "$icd" ]; then
    printf '==> frame_output@lavapipe\n    跳过：没找到 lavapipe 的 ICD（Arch 装 vulkan-swrast）\n'
    return 0
  fi
  local log="$logs/frame_output-lavapipe.log"
  printf '==> frame_output@lavapipe（ICD %s）\n' "$icd"
  local started
  started=$(now_ms)
  if WGPU_BACKEND=vulkan VK_ICD_FILENAMES="$icd" \
    cargo test --test frame_output --features escape-hatch \
      ${cargo_args[@]+"${cargo_args[@]}"} >"$log" 2>&1; then
    local elapsed summary
    elapsed=$(($(now_ms) - started))
    summary=$(grep -E '^test result:' "$log" | tail -1)
    results+=("PASS  frame_output@lavapipe  ${elapsed}ms  ${summary#test result: }")
    printf '    PASS (%d ms) %s\n' "$elapsed" "$summary"
  else
    local elapsed
    elapsed=$(($(now_ms) - started))
    results+=("FAIL  frame_output@lavapipe  ${elapsed}ms  日志：$log")
    printf '    FAIL (%d ms)，日志末尾：\n' "$elapsed" >&2
    tail -60 "$log" >&2
    failed+=("frame_output@lavapipe")
  fi
}

# 前面的用例全绿才值得再跑一遍软件 Vulkan（它只是加强断言，不是独立需求）。
if [ "${#failed[@]}" -eq 0 ]; then
  run_lavapipe
fi

# ── 汇总 ─────────────────────────────────────────────────────────────────
printf '\n---- 集成测试汇总 ----\n'
if [ "${#results[@]}" -gt 0 ]; then printf '  %s\n' "${results[@]}"; fi
ran=${#results[@]}
if [ "$ran" -lt "${#TESTS[@]}" ]; then
  printf '  未执行（前面失败就停了）：%s\n' "${TESTS[@]:$ran}"
fi
if [ "${#failed[@]}" -gt 0 ]; then
  printf '失败：%s\n' "${failed[*]}" >&2
  exit 1
fi
printf '全部通过（%d 个）\n' "$ran"
