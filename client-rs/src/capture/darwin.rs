//! macOS capture backend (ScreenCaptureKit).
//!
//! Fetching `SCShareableContent` itself requires screen recording permission, so
//! permission state can be determined from the preflight
//! (CGPreflightScreenCaptureAccess) and from a failed fetch.

use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use block2::RcBlock;
use image::RgbImage;
use objc2::AllocAnyThread;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2_app_kit::NSWorkspace;
use objc2_core_graphics::{
    CGDataProvider, CGEventSource, CGEventSourceStateID, CGEventType, CGImage,
    CGPreflightScreenCaptureAccess,
};
use objc2_foundation::NSError;
use objc2_screen_capture_kit::{
    SCContentFilter, SCScreenshotManager, SCShareableContent, SCStreamConfiguration, SCWindow,
};

use super::focus;
use super::{ActiveWindow, Capturer};

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(15);

/// kCGAnyInputEventType (any kind of input event). Defined in the headers as `(CGEventType)(~0)`.
const ANY_INPUT_EVENT_TYPE: CGEventType = CGEventType(u32::MAX);

/// Wrapper for moving an ObjC object out of the completion handler's thread.
/// Only use it for objects that are read-only once received.
struct SendRetained<T>(Retained<T>);
unsafe impl<T> Send for SendRetained<T> {}

pub struct DarwinCapturer {
    /// Window picked by the most recent active_window(); grab() looks it up by windowID
    last_window: Option<Retained<SCWindow>>,
}

impl DarwinCapturer {
    pub fn new() -> Self {
        Self { last_window: None }
    }
}

/// Lists the windows currently on screen. Without permission this is where it fails.
fn shareable_content() -> Result<Retained<SCShareableContent>> {
    let (tx, rx) = mpsc::channel::<Result<SendRetained<SCShareableContent>, String>>();
    let block = RcBlock::new(
        move |content: *mut SCShareableContent, error: *mut NSError| {
            let result = if let Some(content) = unsafe { Retained::retain(content) } {
                Ok(SendRetained(content))
            } else {
                Err(error_message(error, "SCShareableContent returned nothing"))
            };
            let _ = tx.send(result);
        },
    );
    unsafe {
        SCShareableContent::getShareableContentExcludingDesktopWindows_onScreenWindowsOnly_completionHandler(
            true, true, &block,
        );
    }
    match rx.recv_timeout(CALLBACK_TIMEOUT) {
        Ok(Ok(content)) => Ok(content.0),
        Ok(Err(msg)) => bail!("cannot get the window list: {msg}"),
        Err(_) => bail!("getting the window list timed out"),
    }
}

/// Finds the window with the matching windowID in the list.
fn find_window(content: &SCShareableContent, window_id: u32) -> Option<Retained<SCWindow>> {
    unsafe { content.windows() }
        .iter()
        .find(|window| unsafe { window.windowID() } == window_id)
}

/// Largest layer-0 window of the given pid. Fallback for when focus cannot be determined.
fn largest_window(content: &SCShareableContent, pid: i32) -> Option<Retained<SCWindow>> {
    let mut best: Option<(Retained<SCWindow>, f64)> = None;
    for window in unsafe { content.windows() }.iter() {
        let owner_pid = unsafe { window.owningApplication() }.map(|a| unsafe { a.processID() });
        if owner_pid != Some(pid) || unsafe { window.windowLayer() } != 0 {
            continue;
        }
        let frame = unsafe { window.frame() };
        let area = frame.size.width * frame.size.height;
        if best.as_ref().is_none_or(|(_, best_area)| area > *best_area) {
            best = Some((window, area));
        }
    }
    best.map(|(window, _)| window)
}

fn error_message(error: *mut NSError, fallback: &str) -> String {
    if error.is_null() {
        fallback.to_string()
    } else {
        unsafe { &*error }.localizedDescription().to_string()
    }
}

impl Capturer for DarwinCapturer {
    fn preflight(&mut self) -> bool {
        CGPreflightScreenCaptureAccess()
    }

    fn idle_seconds(&mut self) -> f64 {
        CGEventSource::seconds_since_last_event_type(
            CGEventSourceStateID::HIDSystemState,
            ANY_INPUT_EVENT_TYPE,
        )
    }

    fn active_window(&mut self) -> Result<ActiveWindow> {
        let workspace = NSWorkspace::sharedWorkspace();
        let front = workspace
            .frontmostApplication()
            .context("cannot get the frontmost app")?;
        let pid = front.processIdentifier();
        let app = front
            .localizedName()
            .map(|s| s.to_string())
            .unwrap_or_default();

        let content = shareable_content()?;
        // Prefer the window the OS considers focused.
        // Fall back to the largest window only when that one cannot be found.
        let focused = focus::focused_window(pid);
        let mut source = "area";
        let mut window = None;
        if let Some(focused) = &focused
            && let Some(found) = find_window(&content, focused.id)
        {
            source = focused.source;
            window = Some(found);
        }
        let window = match window {
            Some(window) => window,
            None => largest_window(&content, pid).context("no active window was found")?,
        };

        let title = unsafe { window.title() }
            .map(|t| t.to_string())
            .unwrap_or_default();
        let window_id = unsafe { window.windowID() };
        self.last_window = Some(window);
        Ok(ActiveWindow {
            app,
            title,
            window_id,
            source,
        })
    }

    fn grab(&mut self, target: &ActiveWindow) -> Result<RgbImage> {
        let window = self
            .last_window
            .as_ref()
            .filter(|w| unsafe { w.windowID() } == target.window_id)
            .context("no target window info to capture (call active_window first)")?;

        let filter = unsafe {
            SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), window)
        };
        // contentRect is in points, so multiply by pointPixelScale to get pixels
        let rect = unsafe { filter.contentRect() };
        let scale = f64::from(unsafe { filter.pointPixelScale() });
        let width = ((rect.size.width * scale).round() as usize).max(1);
        let height = ((rect.size.height * scale).round() as usize).max(1);

        let config: Retained<SCStreamConfiguration> =
            unsafe { msg_send![SCStreamConfiguration::alloc(), init] };
        unsafe {
            config.setWidth(width);
            config.setHeight(height);
            config.setShowsCursor(false);
        }

        let (tx, rx) = mpsc::channel::<Result<RawImage, String>>();
        let block = RcBlock::new(move |image: *mut CGImage, error: *mut NSError| {
            let result = if image.is_null() {
                Err(error_message(error, "CGImage returned nothing"))
            } else {
                copy_pixels(unsafe { &*image })
            };
            let _ = tx.send(result);
        });
        unsafe {
            SCScreenshotManager::captureImageWithFilter_configuration_completionHandler(
                &filter,
                &config,
                Some(&block),
            );
        }
        let raw = match rx.recv_timeout(CALLBACK_TIMEOUT) {
            Ok(Ok(raw)) => raw,
            Ok(Err(msg)) => bail!("capture failed: {msg}"),
            Err(_) => bail!("capture timed out"),
        };
        raw.into_rgb()
    }
}

/// Pixel data carried out of the completion handler (kept in a plain Vec so it is Send).
struct RawImage {
    width: usize,
    height: usize,
    bytes_per_row: usize,
    /// BGRA (SCScreenshotManager returns BGRA for SDR content)
    data: Vec<u8>,
}

fn copy_pixels(image: &CGImage) -> Result<RawImage, String> {
    let width = CGImage::width(Some(image));
    let height = CGImage::height(Some(image));
    let bits_per_pixel = CGImage::bits_per_pixel(Some(image));
    if bits_per_pixel != 32 {
        return Err(format!(
            "unexpected pixel format (bitsPerPixel={bits_per_pixel})"
        ));
    }
    let provider = CGImage::data_provider(Some(image)).ok_or("the CGImage has no DataProvider")?;
    let data = CGDataProvider::data(Some(&provider)).ok_or("cannot get the pixel data")?;
    Ok(RawImage {
        width,
        height,
        bytes_per_row: CGImage::bytes_per_row(Some(image)),
        data: data.to_vec(),
    })
}

impl RawImage {
    fn into_rgb(self) -> Result<RgbImage> {
        if self.data.len() < self.bytes_per_row * self.height || self.bytes_per_row < self.width * 4
        {
            bail!(
                "the pixel data length does not match (len={}, {}x{}, bytesPerRow={})",
                self.data.len(),
                self.width,
                self.height,
                self.bytes_per_row
            );
        }
        let mut rgb = Vec::with_capacity(self.width * self.height * 3);
        for row in 0..self.height {
            let offset = row * self.bytes_per_row;
            for col in 0..self.width {
                let p = offset + col * 4;
                rgb.extend_from_slice(&[self.data[p + 2], self.data[p + 1], self.data[p]]);
            }
        }
        RgbImage::from_raw(self.width as u32, self.height as u32, rgb)
            .context("cannot build an image from the RGB buffer")
    }
}
