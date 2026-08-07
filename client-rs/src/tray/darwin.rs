//! macOS-specific tray handling: the NSApplication event pump, and the menu bar icon that shows
//! the current state.

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask};
use objc2_foundation::{MainThreadMarker, NSDate, NSDefaultRunLoopMode};
use tray_icon::menu::Menu;
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use super::art;

/// tray-icon scales the menu bar image to 18pt tall, so render at twice that for Retina.
const ICON_SIZE: u32 = 36;

pub struct Ui {
    app: Retained<NSApplication>,
}

pub fn init() -> Result<Ui> {
    let mtm = MainThreadMarker::new().context("menubar mode must run on the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    // Live in the menu bar only, with no Dock icon (pairs with LSUIElement in Info.plist)
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.finishLaunching();
    Ok(Ui { app })
}

impl Ui {
    /// Drain the pending AppKit events, waiting until the deadline when there are none.
    pub fn pump(&self, seconds: f64) {
        let deadline = NSDate::dateWithTimeIntervalSinceNow(seconds);
        while let Some(event) = self.app.nextEventMatchingMask_untilDate_inMode_dequeue(
            NSEventMask::Any,
            Some(&deadline),
            unsafe { NSDefaultRunLoopMode },
            true,
        ) {
            self.app.sendEvent(&event);
        }
    }
}

pub fn build_tray(menu: Menu) -> Result<TrayIcon> {
    TrayIconBuilder::new()
        .with_icon(icon(true)?)
        .with_menu(Box::new(menu))
        .build()
        .context("cannot live in the menu bar")
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

/// The same artwork as the app icon: a white camera on the green plate while running, and a pause
/// mark on a grey plate while paused. Not a template image, because going monochrome would drop
/// exactly the color that ties the menu bar to the app icon.
fn icon(running: bool) -> Result<Icon> {
    let (plate, ink) = if running {
        (art::ACTIVE_COLOR, art::camera_ink as art::Ink)
    } else {
        (art::PAUSED_COLOR, art::pause_ink as art::Ink)
    };
    Icon::from_rgba(
        art::render_badge(ICON_SIZE, plate, ink),
        ICON_SIZE,
        ICON_SIZE,
    )
    .context("cannot create the icon")
}

/// The status text already shows as the first menu item, and macOS offers nowhere else to put it.
pub fn set_status_hint(_tray: &TrayIcon, _text: &str) {}
