//! Windows-specific tray handling: the notification area icon, and the thread's message pump.
//!
//! tray-icon creates a hidden window on the calling thread, so unless that same thread keeps
//! pumping messages, neither clicks nor the menu respond. `set_title` does nothing on Windows,
//! so running vs. paused is shown through the icon artwork and the status text through the
//! tooltip.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tray_icon::menu::Menu;
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use windows::Win32::System::Console::{GetConsoleProcessList, GetConsoleWindow};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, SW_HIDE, ShowWindow, TranslateMessage,
};

use super::art;

/// How often we look for messages. This bounds the latency of a menu click.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

pub struct Ui;

pub fn init() -> Result<Ui> {
    hide_own_console();
    Ok(Ui)
}

impl Ui {
    /// Drain the pending window messages, then keep polling at a short interval until the
    /// deadline.
    pub fn pump(&self, seconds: f64) {
        let deadline = Instant::now() + Duration::from_secs_f64(seconds);
        loop {
            let mut msg = MSG::default();
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                // While the settings window is open, give Tab navigation and Enter/Esc the
                // dialog treatment first (messages it does not claim pass through untouched)
                if crate::settings::is_dialog_message(&msg) {
                    continue;
                }
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            if Instant::now() >= deadline {
                return;
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

pub fn build_tray(menu: Menu) -> Result<TrayIcon> {
    TrayIconBuilder::new()
        .with_icon(icon(true)?)
        .with_tooltip(TOOLTIP_PREFIX)
        .with_menu(Box::new(menu))
        .build()
        .context("cannot live in the notification area")
}

pub fn set_running(tray: &TrayIcon, running: bool) {
    match icon(running) {
        Ok(icon) => {
            if let Err(e) = tray.set_icon(Some(icon)) {
                tracing::warn!(event = "tray_icon_error", error = %e);
            }
        }
        Err(e) => tracing::warn!(event = "tray_icon_error", error = %format!("{e:#}")),
    }
}

/// The notification area can only show an icon, so the status text goes in the tooltip.
pub fn set_status_hint(tray: &TrayIcon, text: &str) {
    if let Err(e) = tray.set_tooltip(Some(format!("{TOOLTIP_PREFIX} — {text}"))) {
        tracing::warn!(event = "tray_tooltip_error", error = %e);
    }
}

const TOOLTIP_PREFIX: &str = "Screen Memory";

/// We build as a console app, so launching from Explorer or a shortcut leaves a black console
/// window behind. Hiding it when we were launched from an existing terminal would take away the
/// user's own window, so only hide it when this process is the console's sole attachment.
fn hide_own_console() {
    let console = unsafe { GetConsoleWindow() };
    if console.is_invalid() {
        return;
    }
    let mut pids = [0u32; 2];
    if unsafe { GetConsoleProcessList(&mut pids) } == 1 {
        let _ = unsafe { ShowWindow(console, SW_HIDE) };
    }
}

const ICON_SIZE: u32 = 32;

/// A camera while running, a pause mark while paused. The notification area sits on the taskbar,
/// so the shape goes on transparent rather than on the app icon's green plate. Switching happens
/// rarely enough that we just render on demand.
fn icon(running: bool) -> Result<Icon> {
    let (color, ink) = if running {
        (art::ACTIVE_COLOR, art::camera_ink as art::Ink)
    } else {
        (art::PAUSED_COLOR, art::pause_ink as art::Ink)
    };
    Icon::from_rgba(
        art::render_glyph(ICON_SIZE, color, ink),
        ICON_SIZE,
        ICON_SIZE,
    )
    .context("cannot create the icon")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_are_drawable() {
        assert!(icon(true).is_ok());
        assert!(icon(false).is_ok());
    }
}
