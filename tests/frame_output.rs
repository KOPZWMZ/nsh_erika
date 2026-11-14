//! 出帧路径的端到端测试：**解码 → 合成 → 线性 dma-buf 导出**这条链真的产出了东西。
//!
//! 为什么这条必须存在：其余集成测试（`engine_state` / `open` / `playback` / `shutdown`）
//! 只碰**控制面**（open / play / 状态机 / 退出），没有一条断言"画面出来了"。出帧的终点是
//! `FrameSlot`，只有 [`ErikaVideoSource::frame_snapshot`] 能看到它 —— 在写这条测试之前，
//! 除了真机手验，这条链没有任何回归保护。
//!
//! 断言分两级，**像素读不到也不代表测试可以直接通过**：
//!
//! | 级别 | 断言 | 需要什么 |
//! |---|---|---|
//! | 必需 | 4 秒内出帧；尺寸 / `stride` / `offset` 合法 | 能导出 dma-buf 的 Vulkan 设备 |
//! | 必需 | 连续 5 帧 generation 递增；5 帧里出现 ≥2 块不同的 dma-buf（池在轮转） | 同上（`fstat` 的 inode 当身份） |
//! | 必需 | 改导出尺寸后仍出帧，尺寸跟随（池按新尺寸重建） | 同上 |
//! | 加分 | mmap 读像素：非纯黑 / 非纯白 / 基本不透明；播放 1.2 秒后内容变了 | 驱动允许 mmap dma-buf |
//!
//! 实测（2026-11-14，Mesa 26.2.6 / AMD Renoir）两种后端恰好互补：
//!
//! | 后端 | 结构 / 轮转 / 改尺寸 | mmap 读像素 |
//! |---|---|---|
//! | RADV（硬件 Vulkan） | ✅ | ❌ `EPERM`（导出内存不允许 CPU 映射，两种 prot 都被拒） |
//! | lavapipe（软件 Vulkan） | ✅ | ✅ `black=0 white=0 opaque=16384`，1.2s 后 digest 变化 |
//!
//! 所以像素级是**加分项**：读不到就打一次 WARN 跳过，其余断言照常硬校验 ——
//! 不能让"读不到像素"变成"提前通过"。想拿到像素级证据就跑 lavapipe 那一遍
//! （`scripts/test-integration.sh` 会自动补这一遍）。
//!
//! ⚠️ 前提是一台能把 `R8G8B8A8_UNORM` 线性图像导出成 dma-buf 的 Vulkan 设备：
//!
//! - 真机（RADV / ANV）默认就能过；
//! - 软件 Vulkan 用 loader 层指定，别用 `WGPU_ADAPTER_NAME`（只有 wgpu 的示例辅助函数读它）：
//!   `VK_ICD_FILENAMES=/usr/share/vulkan/icd.d/lvp_icd.x86_64.json cargo test --test frame_output`
//! - 完全没有 Vulkan 设备时打印 `SKIP` 并返回（不红 CI），设
//!   `NSH_ERIKA_REQUIRE_FRAMES=1` 改成硬失败 —— CI 想强制要求"必须能出帧"时用。

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use nsh_erika::{ErikaPlayer, ErikaVideoSource, FrameRequest};

/// 导出的尺寸。故意取小：这条测试只关心"有没有东西"，不在乎分辨率。
const EXPORT: (u32, u32) = (128, 128);

/// 改尺寸后的第二组：dma-buf 池要按新尺寸重建（3 张新 image）。
const EXPORT_RESIZED: (u32, u32) = (192, 108);

/// 连续出帧的取样帧数。池是 3 槽，5 帧足够看到轮转。
const RING_FRAMES: usize = 5;

/// 等一帧的上限。软件 Vulkan（lavapipe）上首帧可能到秒级，给足余量。
const FRAME_TIMEOUT: Duration = Duration::from_secs(4);

/// 引擎线程、音频设备、Vulkan 设备都是进程级独占的：整个文件串行。
static LOCK: Mutex<()> = Mutex::new(());

/// dma-buf 不可 mmap 已经报过没有。驱动拒绝是能力问题，不是偶发 ——
/// 每帧都打一遍会把 `--nocapture` 的输出淹掉（实测两百多行）。
static MMAP_DENIED: AtomicBool = AtomicBool::new(false);

/// 像控件那样"每帧求一次"，直到拿到比 `after` 更新的帧。
///
/// 无头环境里没有 GTK tick，所以请求必须由测试自己发：`request_frame` 会置
/// `want_frame` 并唤醒引擎，引擎下一次 `pump_frame` 才会真的导出。
fn next_frame(
    source: &ErikaVideoSource,
    export: (u32, u32),
    after: u64,
    timeout: Duration,
) -> Option<(u64, Arc<erika::DmabufFrame>)> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        source.request_frame(FrameRequest { export, scale: 1.0 });
        let (generation, frame) = source.frame_snapshot();
        if generation > after
            && let Some(frame) = frame
        {
            return Some((generation, frame));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

/// 出不了帧时的处理：**缺 Vulkan 是环境问题**（打印 SKIP），其余是 bug（panic）。
fn report_missing_frame(player: &ErikaPlayer, what: &str) {
    let reason = player
        .status()
        .error
        .or_else(|| player.last_error())
        .unwrap_or_else(|| "没有错误信息".to_owned());
    let strict = std::env::var_os("NSH_ERIKA_REQUIRE_FRAMES")
        .is_some_and(|value| !value.is_empty() && value != "0");
    let export_unavailable = ["dma-buf", "DMA_BUF", "Vulkan", "vulkan"]
        .iter()
        .any(|needle| reason.contains(needle));
    if export_unavailable && !strict {
        eprintln!(
            "SKIP: {what}：本机没有能导出 dma-buf 的 Vulkan 设备 —— {reason}\n\
             （Arch 上装 Mesa 的软件 Vulkan 即可：pacman -S vulkan-swrast；\n\
             想让它算失败就设 NSH_ERIKA_REQUIRE_FRAMES=1）"
        );
        return;
    }
    panic!("{what}：4 秒内没出帧，也没有可归因到环境的错误（err={reason}）");
}

/// 帧的几何必须跟请求一致。
fn assert_geometry(frame: &erika::DmabufFrame, export: (u32, u32), what: &str) {
    assert_eq!(
        (frame.width(), frame.height()),
        export,
        "{what}：导出尺寸应当就是请求的尺寸"
    );
    assert!(
        frame.stride() >= export.0 * 4,
        "{what}：stride={} 装不下一行 {} 像素",
        frame.stride(),
        export.0
    );
    assert!(
        frame.offset() < frame.stride(),
        "{what}：offset={} 应小于 stride={}",
        frame.offset(),
        frame.stride()
    );
}

/// dma-buf 的身份 `(st_dev, st_ino)`。
///
/// 同一个 buffer 的 `dup` 出来的 fd 共享 inode，所以这个能区分"池里的哪一块"，
/// 又不会被 `try_clone_fd` 的 fd 编号骗到。`None` = `fstat` 失败。
fn dma_buf_identity(frame: &erika::DmabufFrame) -> Option<(u64, u64)> {
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `stat` 是有效的可写结构，fd 由帧自己持有。
    let result = unsafe { libc::fstat(frame.raw_fd(), &mut stat) };
    (result == 0).then_some((stat.st_dev as u64, stat.st_ino as u64))
}

/// 一帧的像素统计。`digest` 用来判断"两帧内容是否不同"。
#[derive(Debug)]
struct PixelStats {
    pixels: usize,
    /// RGB 全 0 的像素数（纯黑）。
    black: usize,
    /// RGB 全 255 的像素数（纯白）。
    white: usize,
    /// alpha == 255 的像素数。
    opaque: usize,
    digest: u64,
}

/// mmap 块 dma-buf，读像素。返回 `None` = 这个驱动不允许映射（不是错误）。
///
/// 实测 RADV 上 `PROT_READ` 会拿到 `EPERM`，所以再试一次可写映射 —— 两种都失败就放弃。
fn read_pixels(frame: &erika::DmabufFrame) -> Option<PixelStats> {
    // 已知本驱动不给映射：别再试、也别再刷日志。
    if MMAP_DENIED.load(Ordering::Relaxed) {
        return None;
    }
    let offset = frame.offset() as usize;
    let stride = frame.stride() as usize;
    let (width, height) = (frame.width() as usize, frame.height() as usize);
    let len = offset + stride * height;

    let mut mapped = libc::MAP_FAILED;
    let mut errors = Vec::new();
    for prot in [libc::PROT_READ, libc::PROT_READ | libc::PROT_WRITE] {
        // SAFETY: 只依赖 fd 与长度；返回 MAP_FAILED 时不做任何解引用。
        let attempt = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                prot,
                libc::MAP_SHARED,
                frame.raw_fd(),
                0,
            )
        };
        if attempt != libc::MAP_FAILED {
            mapped = attempt;
            break;
        }
        errors.push(format!(
            "prot={prot:#x}: {}",
            std::io::Error::last_os_error()
        ));
    }
    if mapped == libc::MAP_FAILED {
        MMAP_DENIED.store(true, Ordering::Relaxed);
        eprintln!(
            "WARN: dma-buf mmap 失败（{}）—— 后续帧不再重复提示",
            errors.join("，")
        );
        return None;
    }

    // SAFETY: `mapped` 指向 `len` 字节的可读映射，生命周期到下面的 munmap 为止。
    let bytes = unsafe { std::slice::from_raw_parts(mapped as *const u8, len) };
    let mut stats = PixelStats {
        pixels: 0,
        black: 0,
        white: 0,
        opaque: 0,
        // FNV-1a 的初始值，只为"能不能区分两帧"，不做密码学用途。
        digest: 0xcbf2_9ce4_8422_2325,
    };
    for row in 0..height {
        let start = offset + row * stride;
        let line = &bytes[start..start + width * 4];
        for pixel in line.as_chunks::<4>().0 {
            let (r, g, b, a) = (pixel[0], pixel[1], pixel[2], pixel[3]);
            stats.pixels += 1;
            if r == 0 && g == 0 && b == 0 {
                stats.black += 1;
            }
            if r == 255 && g == 255 && b == 255 {
                stats.white += 1;
            }
            if a == 255 {
                stats.opaque += 1;
            }
            for byte in pixel {
                stats.digest = (stats.digest ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }

    // SAFETY: 同一块映射、同样的长度，解除一次。
    unsafe { libc::munmap(mapped, len) };
    Some(stats)
}

#[test]
fn decodes_and_exports_pixels() {
    let _guard = LOCK.lock().unwrap_or_else(|error| error.into_inner());

    let player = new_player();
    player.open(media(SILENT_MEDIA));
    wait(
        &player,
        |p| p.state() == erika::PlayerState::Ready,
        "open 完成",
    );

    let source = player.video_source();
    // 只有真的在播，解码器才会往前吐帧（Ready 状态下拿不到后续帧）。
    player.play();

    // ── 1. 首帧 + 几何 ─────────────────────────────────────────────────────
    let Some((mut generation, frame)) = next_frame(&source, EXPORT, 0, FRAME_TIMEOUT) else {
        report_missing_frame(&player, "首帧");
        return;
    };
    assert_geometry(&frame, EXPORT, "首帧");

    // ── 2. 像素（加分项，读不到不算失败）──────────────────────────────────
    let first_pixels = read_pixels(&frame);
    match &first_pixels {
        Some(stats) => {
            eprintln!(
                "首帧 stats: pixels={} black={} white={} opaque={} digest={:#x}",
                stats.pixels, stats.black, stats.white, stats.opaque, stats.digest
            );
            assert_eq!(
                stats.pixels,
                (EXPORT.0 * EXPORT.1) as usize,
                "读到的像素数应等于导出尺寸"
            );
            assert!(
                stats.black < stats.pixels,
                "整帧纯黑：解码 / 合成没有出内容"
            );
            assert!(
                stats.white < stats.pixels,
                "整帧纯白：采样或颜色通道顺序可能不对"
            );
            assert!(
                stats.opaque * 100 >= stats.pixels * 99,
                "合成结果应当基本不透明（opaque={}/{}）",
                stats.opaque,
                stats.pixels
            );
        }
        None => eprintln!(
            "WARN: 本驱动不允许 mmap dma-buf → 跳过像素级断言（几何 / 轮转 / 改尺寸仍然硬校验）"
        ),
    }

    // ── 3. 连续出帧 + dma-buf 池轮转 ───────────────────────────────────────
    let mut identities = BTreeSet::new();
    if let Some(identity) = dma_buf_identity(&frame) {
        identities.insert(identity);
    }
    for index in 0..RING_FRAMES {
        let Some((next_generation, frame)) = next_frame(&source, EXPORT, generation, FRAME_TIMEOUT)
        else {
            panic!(
                "连续出帧失败：第 {} 帧没来（池是 3 槽轮转，说明 export 卡住了）",
                index + 2
            );
        };
        assert!(
            next_generation > generation,
            "generation 应严格递增：{generation} → {next_generation}"
        );
        generation = next_generation;
        assert_geometry(&frame, EXPORT, &format!("第 {} 帧", index + 2));
        if let Some(identity) = dma_buf_identity(&frame) {
            identities.insert(identity);
        }
    }
    match identities.len() {
        0 => eprintln!("WARN: fstat 拿不到 dma-buf 身份，跳过轮转断言"),
        1 => panic!(
            "连续 {} 帧都是同一块 dma-buf：池没有轮转（identity={:?}）",
            RING_FRAMES + 1,
            identities
        ),
        count => eprintln!(
            "dma-buf 池轮转正常：{} 帧里出现了 {} 块不同的 buffer",
            RING_FRAMES + 1,
            count
        ),
    }

    // ── 4. 播放推进：内容要变（像素不可读时退化为"帧在推进"）──────────────
    let deadline = Instant::now() + Duration::from_millis(1200);
    let mut later_pixels = None;
    while Instant::now() < deadline {
        let Some((next_generation, frame)) =
            next_frame(&source, EXPORT, generation, Duration::from_millis(200))
        else {
            continue;
        };
        generation = next_generation;
        if let Some(stats) = read_pixels(&frame) {
            later_pixels = Some(stats);
        }
    }
    match (first_pixels.as_ref(), later_pixels) {
        (Some(first), Some(later)) => {
            eprintln!("1.2s 后 digest={:#x}", later.digest);
            assert_ne!(
                later.digest, first.digest,
                "播放 1.2 秒后画面完全没变：可能一直拿到同一帧"
            );
        }
        _ => eprintln!(
            "WARN: 像素不可读，跳过\"画面在动\"的断言（上面 generation + 不同 buffer 已证明在出新帧）"
        ),
    }

    // ── 5. 改导出尺寸：池按新尺寸重建并继续出帧 ────────────────────────────
    let Some((_, resized)) = next_frame(&source, EXPORT_RESIZED, generation, FRAME_TIMEOUT) else {
        panic!("改导出尺寸后没出帧：dma-buf 池没按新尺寸重建？");
    };
    assert_geometry(&resized, EXPORT_RESIZED, "改尺寸后");

    eprintln!(
        "出帧路径 OK：首帧 → 轮转({} 帧) → 播放推进 → 改尺寸，{}",
        RING_FRAMES + 1,
        if first_pixels.is_some() {
            "像素级断言全过"
        } else {
            "像素级被驱动拒绝（已降级）"
        }
    );
}
