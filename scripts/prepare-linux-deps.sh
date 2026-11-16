#!/usr/bin/env bash
# 生成 Linux 编译 erika 需要的**本机前置**（放在 .vendor/，不进 git）：补丁版 libass。
#
#   补丁版 libass 0.17.5 —— `erika/build.rs` 断言 `ERIKA_LIBASS_DIR`（或默认 dist 路径）下
#   存在 `lib/libass.a`，因为系统 libass 缺"有序内存字体回退"那个补丁。由 liberika 仓里的
#   `scripts/build_linux_libass.sh` 生成（meson/ninja/patch）。
#   顺带把 `.cargo/config.toml`（也不进 git）写出来，只放 `ERIKA_LIBASS_DIR`。
#
# 另一项前置**不在这里**：打补丁的 `wgpu-hal` 走 `Cargo.toml` 的 `[patch.crates-io]`
# （git 指向同一个 tag）。liberika `docs/linux.zh.md`「仓库外的 Cargo workspace」明确要求
# 这么做，并说明**不要**用 Cargo `paths` 覆盖 —— 那是解析后硬改源码位置，会带
# "path override … has altered the original list of dependencies" 警告，而且配置不入 git，
# 别人 clone 下来编不过。
#
# 用法：
#   bash scripts/prepare-linux-deps.sh           # 幂等，已就绪就跳过
#   bash scripts/prepare-linux-deps.sh --force   # 强制重建
#
# 幂等依据是 `.vendor/erika-deps.stamp`（tag + libass 补丁集 hash）：
# **libass 的补丁集会随 erika 版本变**（v0.3.6 侧是 288cd48…，v0.3.7 是 c55e80b4…），
# 复用旧 `.a` 能编过但会静默退回旧行为 —— 所以 patch hash 变了必须重建。
#
# 需要：bash、meson、ninja、patch、pkg-config、curl、sha256sum、
# freetype2/harfbuzz/fribidi/fontconfig 的开发包（`pkg-config --exists` 要能过）。
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$root"

force=0
for arg in "$@"; do
  case "$arg" in
    --force) force=1 ;;
    -h | --help) awk 'NR == 1 { next } /^#/ { sub(/^# ?/, ""); print; next } { exit }' "$0"; exit 0 ;;
    *) echo "未知参数：$arg（只支持 --force）" >&2; exit 2 ;;
  esac
done

vendor="$root/.vendor"
stamp="$vendor/erika-deps.stamp"
cfg="$root/.cargo/config.toml"

say() { printf '==> %s\n' "$*"; }
die() {
  printf '错误：%s\n' "$*" >&2
  exit 1
}

# ── 0. 依赖坐标的唯一事实来源：Cargo.toml 里的 erika tag ─────────────────────
tag=$(sed -n 's/.*tag = "\(v[0-9][^"]*\)".*/\1/p' "$root/Cargo.toml" | head -1)
[ -n "$tag" ] || die "Cargo.toml 里没找到 erika 的 tag（应为 tag = \"v0.3.x\"）"
say "erika tag: $tag"

# wgpu-hal 的补丁必须和 erika 同一 tag，否则 HAL 与引擎可能来自不同 commit。
patch_tag=$(awk '/^\[patch\.crates-io\]/ { seen = 1 } seen && /^wgpu-hal/ { print; exit }' \
  "$root/Cargo.toml" | sed -n 's/.*tag = "\([^"]*\)".*/\1/p')
if [ -z "$patch_tag" ]; then
  die "Cargo.toml 的 [patch.crates-io] 里没有 wgpu-hal = { git = …, tag = … }；
       Linux 的 Vulkan 直接导入需要它（见 liberika docs/linux.zh.md）"
fi
[ "$patch_tag" = "$tag" ] || die "wgpu-hal 的 tag（$patch_tag）必须和 erika 的 tag（$tag）一致"
say "wgpu-hal patch: $patch_tag（同一 tag，OK）"

# ── 1. 找到 cargo 给 erika 拉的 git checkout ────────────────────────────────
# 不用 `cargo metadata`：它会把所有平台的依赖都下载/解析一遍（CI 里多一层失败面）。
# 改成读 Cargo.lock 里的 commit，再去 cargo 的 checkouts 目录里对号；对不上就退回
# "workspace 版本号等于 tag" 的兜底匹配（多版本并存时取最新的那个）。
cargo_home=${CARGO_HOME:-$HOME/.cargo}
rev=$(awk '/^name = "erika"$/ { seen = 1 } seen && /^source = "git\+/ { sub(/.*#/, ""); gsub(/"/, ""); print; exit }' \
  "$root/Cargo.lock")
rev_short=${rev:0:7}
version=${tag#v}
say "erika rev: ${rev:-未知}"

find_checkout() {
  local dir best=""
  shopt -s nullglob
  for dir in "$cargo_home"/git/checkouts/liberika-*/*; do
    [ -f "$dir/scripts/build_linux_libass.sh" ] || continue
    if [ -n "$rev_short" ] && [ "$(basename "$dir")" = "$rev_short" ]; then
      printf '%s\n' "$dir"
      return 0
    fi
    if grep -qxF "version = \"$version\"" "$dir/Cargo.toml"; then
      if [ -z "$best" ] || [ "$dir" -nt "$best" ]; then best=$dir; fi
    fi
  done
  [ -n "$best" ] && printf '%s\n' "$best"
}

checkout=$(find_checkout)
if [ -z "$checkout" ]; then
  say "还没拉过 erika，先 cargo fetch 一次"
  cargo fetch --manifest-path "$root/Cargo.toml" || die "cargo fetch 失败（网络或镜像问题）"
  checkout=$(find_checkout)
fi
[ -n "$checkout" ] || die "在 $cargo_home/git/checkouts/liberika-*/ 下找不到 erika $tag 的 checkout"
say "checkout: $checkout"

# ── 2. .cargo/config.toml（不进 git）─────────────────────────────────────
want_cfg='# 由 scripts/prepare-linux-deps.sh 生成：本机构建前置，不进 git。
# 只放补丁版 libass 的路径；wgpu-hal 的补丁在 Cargo.toml 的 [patch.crates-io] 里。
[env]
ERIKA_LIBASS_DIR = { value = ".vendor/libass", relative = true }'
if [ ! -f "$cfg" ]; then
  say "写 .cargo/config.toml"
  mkdir -p "$root/.cargo"
  printf '%s\n' "$want_cfg" >"$cfg"
elif grep -q '^paths' "$cfg"; then
  die "$cfg 里还有 Cargo \`paths\` 覆盖（wgpu-hal 已改用 Cargo.toml 的 [patch.crates-io]）。
       请删掉 paths 那一行，只留 [env] ERIKA_LIBASS_DIR = { value = \".vendor/libass\", relative = true }"
elif ! grep -qF 'ERIKA_LIBASS_DIR' "$cfg"; then
  echo "错误：$cfg 已存在但没有设 ERIKA_LIBASS_DIR，为免覆盖你的配置，请手工补上：" >&2
  printf '%s\n\n' "$want_cfg" >&2
  exit 1
fi

# ── 3. 幂等判据：tag + libass 补丁集 hash ────────────────────────────────────
patch_hash=$(cat "$checkout"/third_party/patches/libass-*/*.patch | sha256sum | cut -d' ' -f1)
want="$tag $patch_hash"
if [ "$force" -eq 0 ] && [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$want" ] &&
  [ -f "$vendor/libass/lib/libass.a" ]; then
  say "已是最新（$want），跳过；要重建加 --force"
  exit 0
fi

mkdir -p "$vendor"

# ── 4. .vendor/libass ────────────────────────────────────────────────────
# 上游脚本按 <ROOT>/scripts 与 <ROOT>/third_party 的相对布局工作，所以给它一个
# scratch 根：脚本、补丁、可选缓存都拷进去，产物再搬回 .vendor/。
scratch="${NSH_ERIKA_DEPS_DIR:-${CARGO_TARGET_DIR:-$root/target}}/erika-deps/libass-$tag"
say "在 $scratch 构建补丁版 libass"
mkdir -p "$scratch/scripts" "$scratch/third_party/cache" "$scratch/third_party/patches"
cp "$checkout/scripts/build_linux_libass.sh" "$scratch/scripts/"
cp -a "$checkout"/third_party/patches/libass-* "$scratch/third_party/patches/"
# 有源码缓存就直接用，省一次下载（cargo 的 checkout 里通常没有，无所谓）
for cached in "$checkout"/third_party/cache/libass-*.tar.xz; do
  [ -f "$cached" ] && cp -n "$cached" "$scratch/third_party/cache/"
done

if ! bash "$scratch/scripts/build_linux_libass.sh" >"$scratch/build.log" 2>&1; then
  echo "---- $scratch/build.log（末尾 40 行）----" >&2
  tail -40 "$scratch/build.log" >&2
  die "libass 构建失败（完整日志见上面路径）"
fi

dist=$(ls -d "$scratch"/third_party/dist/*/system/libass 2>/dev/null | head -1)
[ -n "$dist" ] || die "构建完但没找到 $scratch/third_party/dist/*/system/libass"
say "安装补丁版 libass → .vendor/libass（来自 $dist）"
rm -rf "$vendor/libass.new"
cp -a "$dist" "$vendor/libass.new"
rm -rf "$vendor/libass"
mv "$vendor/libass.new" "$vendor/libass"

# ── 5. 收尾 ───────────────────────────────────────────────────────────────
printf '%s\n' "$want" >"$stamp"
say "完成："
printf '    libass    %s\n' "$(du -h "$vendor/libass/lib/libass.a" | cut -f1)"
printf '    wgpu-hal  由 Cargo.toml 的 [patch.crates-io] 提供（%s）\n' "$patch_tag"
printf '    stamp     %s\n' "$want"
printf '\n现在可以：cargo check --all-targets\n'
