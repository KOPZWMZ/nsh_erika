#!/usr/bin/env bash
# 生成 Linux 编译 erika 需要的两个本地前置（都放在 .vendor/，不进 git）：
#
#   1) .vendor/wgpu-hal —— erika 的 VA-API 直接导入用的那个 fork。
#      `linux_vulkan.rs::wrap_plane` 调 `Texture::set_external_initial_usage`，
#      这个方法只在 liberika 仓内的 `third_party/wgpu-hal` 里（wgpu-hal 29.0.3 +
#      `initial_usage` 字段）。上游只用 workspace 级 `[patch.crates-io]` 接它，
#      cargo git 依赖传不出来 → 外部消费者必须自己覆盖。
#   2) .vendor/libass   —— `erika/build.rs` 断言存在的补丁版 libass 0.17.5，
#      由 liberika 的 `scripts/build_linux_libass.sh` 生成（meson/ninja/patch）。
#      系统 libass 不能替代：缺"有序内存字体回退"补丁。
#
# 顺带确保 `.cargo/config.toml`（同样不进 git）把它们接上：
#   paths = [".vendor/wgpu-hal"] + [env] ERIKA_LIBASS_DIR = .vendor/libass
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
    [ -f "$dir/third_party/wgpu-hal/Cargo.toml" ] || continue
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
# 放在幂等判断之前：.vendor 齐全但配置没接上时，不能静默跳过（否则编出来的是
# crates.io 的 wgpu-hal，报的却是 erika 里的 E0599）。
want_cfg='# 由 scripts/prepare-linux-deps.sh 生成：本机构建前置，不进 git。
# 两项都指向 .vendor/，见该脚本顶部注释。
paths = [".vendor/wgpu-hal"]

[env]
ERIKA_LIBASS_DIR = { value = ".vendor/libass", relative = true }'
if [ ! -f "$cfg" ]; then
  say "写 .cargo/config.toml"
  mkdir -p "$root/.cargo"
  printf '%s\n' "$want_cfg" >"$cfg"
elif ! grep -qF '.vendor/wgpu-hal' "$cfg" || ! grep -qF 'ERIKA_LIBASS_DIR' "$cfg"; then
  echo "错误：$cfg 已存在但没有接上 .vendor/，为免覆盖你的配置，请手工补上：" >&2
  printf '%s\n\n' "$want_cfg" >&2
  exit 1
fi

# ── 3. 幂等判据：tag + libass 补丁集 hash ────────────────────────────────────
patch_hash=$(cat "$checkout"/third_party/patches/libass-*/*.patch | sha256sum | cut -d' ' -f1)
want="$tag $patch_hash"
if [ "$force" -eq 0 ] && [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$want" ] &&
  [ -f "$vendor/libass/lib/libass.a" ] && [ -f "$vendor/wgpu-hal/Cargo.toml" ]; then
  say "已是最新（$want），跳过；要重建加 --force"
  exit 0
fi

mkdir -p "$vendor"

# ── 4. .vendor/wgpu-hal ───────────────────────────────────────────────────
say "拷贝补丁版 wgpu-hal → .vendor/wgpu-hal"
rm -rf "$vendor/wgpu-hal.new"
cp -a "$checkout/third_party/wgpu-hal" "$vendor/wgpu-hal.new"
rm -rf "$vendor/wgpu-hal"
mv "$vendor/wgpu-hal.new" "$vendor/wgpu-hal"

# ── 5. .vendor/libass ────────────────────────────────────────────────────
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

# ── 6. 收尾 ───────────────────────────────────────────────────────────────
printf '%s\n' "$want" >"$stamp"
say "完成："
printf '    wgpu-hal  %s\n' "$(sed -n 's/^version = "\(.*\)"/\1/p' "$vendor/wgpu-hal/Cargo.toml" | head -1)"
printf '    libass    %s\n' "$(du -h "$vendor/libass/lib/libass.a" | cut -f1)"
printf '    stamp     %s\n' "$want"
printf '\n现在可以：cargo check --all-targets\n'
