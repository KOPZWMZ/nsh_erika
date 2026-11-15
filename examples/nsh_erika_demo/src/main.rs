//! Runs the Rust application directly on the machine's native backend.

// #![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "linux")]
use nsh_platform_linux as platform;
#[cfg(target_os = "macos")]
use nsh_platform_macos as platform;
#[cfg(target_os = "windows")]
use nsh_platform_windows as platform;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn main() -> impl std::process::Termination {
    env_logger::init();
    // `cargo run` 不经过 nsh 生成的宿主，所以原生控件要在这里自己注册
    // （`nsh run` 走的是 `#[native_components(linux)]` 那个入口，不用这句）。
    let options = platform::AppOptions {
        title: "nsh_erika_demo".to_owned(),
        app_id: "com.example.nsh_erika_demo".to_owned(),
        size: (1280, 720),
        ..Default::default()
    };
    nsh_erika::register(&options.components);
    platform::run_app(
        platform::AppSource::Rust(platform::Root::of::<nsh_app_erika_demo::App>()),
        options,
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!(
    "Run this binary on macOS, Windows or Linux; use the TSX entry with nsh run for iOS and Android."
);
