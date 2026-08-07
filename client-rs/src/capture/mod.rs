//! Types shared by the per-OS capture backends.

use anyhow::Result;
use image::RgbImage;

/// Information about the frontmost window.
#[derive(Debug, Clone)]
pub struct ActiveWindow {
    /// Application name
    pub app: String,
    /// Window title, or "" when it cannot be read
    pub title: String,
    /// The OS window identifier (CGWindowID on macOS). Unread on Windows, where
    /// capture uses last_hwnd instead
    #[cfg_attr(windows, allow(dead_code))]
    pub window_id: u32,
    /// How this window was picked. Logged so the choice can be reviewed afterwards
    pub source: &'static str,
}

pub trait Capturer {
    /// Permission check at startup. True when screen recording is permitted.
    fn preflight(&mut self) -> bool;

    /// Seconds elapsed since the last input.
    fn idle_seconds(&mut self) -> f64;

    /// Information about the frontmost window.
    fn active_window(&mut self) -> Result<ActiveWindow>;

    /// Captures the window returned by the preceding active_window() call.
    fn grab(&mut self, window: &ActiveWindow) -> Result<RgbImage>;
}

#[cfg(target_os = "macos")]
pub mod darwin;

#[cfg(target_os = "macos")]
mod focus;

/// Whether the Accessibility permission is granted. Without it the OS's own focus
/// determination is unavailable.
#[cfg(target_os = "macos")]
pub fn accessibility_trusted() -> bool {
    focus::accessibility_trusted()
}

#[cfg(target_os = "macos")]
pub fn make_capturer() -> impl Capturer {
    darwin::DarwinCapturer::new()
}

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "windows")]
pub fn make_capturer() -> impl Capturer {
    windows::WindowsCapturer::new()
}
