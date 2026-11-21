#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
mod editor;
#[cfg(target_os = "macos")]
mod preferences;
#[cfg(target_os = "macos")]
mod shortcut;
#[cfg(target_os = "macos")]
mod gif_capture;
#[cfg(target_os = "macos")]
mod gif_window;
#[cfg(target_os = "macos")]
mod localization;

fn main() {
    #[cfg(target_os = "macos")]
    mac::run();
    #[cfg(not(target_os = "macos"))]
    compile_error!("QingJie currently supports macOS only.");
}
