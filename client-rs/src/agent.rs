//! The capture loop itself. Used by both the headless run and the menu bar app.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use image::RgbImage;
use image::imageops::FilterType;

use crate::auth::CognitoAuth;
use crate::capture::{Capturer, make_capturer};
use crate::config::{Config, Mode, state_dir};
use crate::dedupe::{dhash, hamming};
use crate::exclude::ExcludeRules;
use crate::local::{LocalStore, local_dir};
use crate::spool::{
    MAX_SPOOL_FILES, Metadata, Spool, build_metadata, build_text_metadata, json_key,
};
use crate::uploader::Uploader;

pub const MAX_LONG_SIDE: u32 = 1568;
pub const WEBP_QUALITY: f32 = 80.0;

#[cfg(target_os = "macos")]
const ACCESSIBILITY_GUIDE: &str = "[screen-memory] Accessibility permission is missing. Capture keeps running, but the OS\n  cannot be asked which window has focus, so the frontmost window is used instead.\n  Allow Screen Memory under System Settings > Privacy & Security > Accessibility.";

const PERMISSION_GUIDE: &str = "[screen-memory] Screen Recording permission is missing, so nothing can be captured.\n  Allow Screen Memory under System Settings > Privacy & Security >\n  Screen & System Audio Recording, then restart the app.";

pub fn to_webp(img: &RgbImage) -> Result<Vec<u8>> {
    let long_side = img.width().max(img.height());
    let resized;
    let img = if long_side > MAX_LONG_SIDE {
        let scale = f64::from(MAX_LONG_SIDE) / f64::from(long_side);
        let w = (f64::from(img.width()) * scale).round() as u32;
        let h = (f64::from(img.height()) * scale).round() as u32;
        resized = image::imageops::resize(img, w.max(1), h.max(1), FilterType::Lanczos3);
        &resized
    } else {
        img
    };
    let encoder = webp::Encoder::from_rgb(img.as_raw(), img.width(), img.height());
    Ok(encoder.encode(WEBP_QUALITY).to_vec())
}

/// State shared between the capture thread and the menu bar (tray.rs). All atomics.
#[derive(Default)]
struct Shared {
    paused: AtomicBool,
    stopped: AtomicBool,
    /// True when preflight found no screen recording permission
    permission_denied: AtomicBool,
    /// Unix seconds of the most recent capture (written to a sink). 0 means nothing captured yet
    last_captured_epoch: AtomicI64,
}

/// Where records are written. The mode never changes while running; changing it
/// recreates the agent, since local-only mode never starts the uploader at all.
enum Sink {
    /// Upload the image or OCR text to S3 through the spool
    Spool(Arc<Spool>),
    /// Keep records in a local JSONL file instead of sending them to AWS
    Local(LocalStore),
}

/// Ties the capture thread and the uploader together. Controlled via pause() / resume() / stop().
pub struct Agent {
    shared: Arc<Shared>,
    /// None in local-only mode
    uploader: Option<Uploader>,
    mode: Mode,
}

impl Agent {
    /// Modes that send to the cloud (Image / Ocr) require auth and validate the AWS settings.
    /// Local-only mode (Local) ignores auth and never starts the uploader.
    pub fn start(cfg: Config, auth: Option<Arc<CognitoAuth>>, mode: Mode) -> Result<Agent> {
        let rules = ExcludeRules::new(&cfg.exclude_apps, &cfg.exclude_title_patterns)?;
        let (sink, uploader) = if mode.uploads() {
            let aws = cfg.aws()?;
            let auth = auth.context("the cloud-sending modes require a login")?;
            let spool = Arc::new(Spool::new(state_dir().join("spool"), MAX_SPOOL_FILES)?);
            let uploader = Uploader::start(Arc::clone(&spool), aws.bucket, aws.region, auth);
            (Sink::Spool(spool), Some(uploader))
        } else {
            (Sink::Local(LocalStore::new(local_dir())?), None)
        };
        let shared = Arc::new(Shared::default());
        {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("capture".into())
                .spawn(move || capture_loop(&cfg, &rules, &sink, &shared, mode))
                .context("cannot start the capture thread")?;
        }
        Ok(Agent {
            shared,
            uploader,
            mode,
        })
    }

    /// Used by the tray app (tray/). Never called in headless mode.
    pub fn pause(&self) {
        self.shared.paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.shared.paused.store(false, Ordering::Relaxed);
    }

    pub fn paused(&self) -> bool {
        self.shared.paused.load(Ordering::Relaxed)
    }

    pub fn permission_denied(&self) -> bool {
        self.shared.permission_denied.load(Ordering::Relaxed)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn last_captured_at(&self) -> Option<DateTime<Local>> {
        match self.shared.last_captured_epoch.load(Ordering::Relaxed) {
            0 => None,
            epoch => Local.timestamp_opt(epoch, 0).single(),
        }
    }

    pub fn stop(&mut self) {
        self.shared.stopped.store(true, Ordering::Relaxed);
        if let Some(uploader) = &mut self.uploader {
            uploader.stop();
        }
    }
}

/// Waits for the next minute boundary (second 0 of each minute). Returns true if stopped.
fn wait_for_minute_boundary(stopped: &AtomicBool) -> bool {
    let remaining_ms = 60_000 - (Utc::now().timestamp_millis().rem_euclid(60_000));
    let deadline = std::time::Instant::now() + Duration::from_millis(remaining_ms as u64);
    while std::time::Instant::now() < deadline {
        if stopped.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    stopped.load(Ordering::Relaxed)
}

fn capture_loop(cfg: &Config, rules: &ExcludeRules, sink: &Sink, shared: &Shared, mode: Mode) {
    let mut capturer = make_capturer();
    if !capturer.preflight() {
        shared.permission_denied.store(true, Ordering::Relaxed);
        tracing::warn!(event = "permission_denied");
        eprintln!("{PERMISSION_GUIDE}");
    }
    #[cfg(target_os = "macos")]
    if !crate::capture::accessibility_trusted() {
        tracing::warn!(event = "accessibility_denied");
        eprintln!("{ACCESSIBILITY_GUIDE}");
    }
    let mut prev_hash: Option<u64> = None;
    loop {
        if wait_for_minute_boundary(&shared.stopped) {
            return;
        }
        if shared.paused.load(Ordering::Relaxed) {
            continue;
        }
        match capture_once(cfg, rules, sink, &mut capturer, &mut prev_hash, mode) {
            Ok(Some(captured_at)) => shared
                .last_captured_epoch
                .store(captured_at.timestamp(), Ordering::Relaxed),
            Ok(None) => {}
            Err(e) => tracing::warn!(event = "capture_error", error = %format!("{e:#}")),
        }
    }
}

/// Returns the capture time once a capture has been written. None when the capture was skipped.
fn capture_once(
    cfg: &Config,
    rules: &ExcludeRules,
    sink: &Sink,
    capturer: &mut impl Capturer,
    prev_hash: &mut Option<u64>,
    mode: Mode,
) -> Result<Option<DateTime<Utc>>> {
    let idle = capturer.idle_seconds();
    if idle > cfg.idle_threshold_seconds as f64 {
        tracing::info!(event = "skipped_idle", idleSeconds = idle.round() as i64);
        return Ok(None);
    }

    let window = capturer.active_window()?;
    if rules.matches(&window.app, &window.title) {
        tracing::info!(event = "skipped_excluded", app = %window.app);
        return Ok(None);
    }

    let captured_at = Utc::now();
    let img = capturer.grab(&window)?;
    let hash = dhash(&img);
    if let Some(prev) = *prev_hash
        && hamming(prev, hash) <= cfg.dhash_threshold
    {
        tracing::info!(event = "skipped_dup", app = %window.app);
        return Ok(None);
    }

    if mode == Mode::Image {
        let Sink::Spool(spool) = sink else {
            anyhow::bail!("image mode is paired with a local sink (internal inconsistency)");
        };
        let meta = build_metadata(&cfg.device_id, captured_at, &window.app, &window.title);
        spool.write(&to_webp(&img)?, &meta, captured_at)?;
        // Only kept captures update this; using a skipped image's hash would let
        // slow, gradual changes slip past the deduper
        *prev_hash = Some(hash);
        tracing::info!(
            event = "captured",
            key = %meta.image_key.as_deref().unwrap_or_default(),
            app = %window.app,
            source = window.source,
            mode = "image",
        );
        return Ok(Some(captured_at));
    }

    // OCR mode and local-only mode. Keep the record even when OCR fails, since
    // the app name and window title alone are still worth analyzing
    let text = crate::ocr::recognize(&img).unwrap_or_else(|e| {
        tracing::warn!(event = "ocr_error", error = %format!("{e:#}"));
        String::new()
    });
    let chars = text.chars().count();
    let meta = build_text_metadata(
        &cfg.device_id,
        captured_at,
        &window.app,
        &window.title,
        text,
    );
    write_text_record(sink, &meta, captured_at)?;
    *prev_hash = Some(hash);
    tracing::info!(
        event = "captured",
        key = %json_key(&cfg.device_id, captured_at),
        app = %window.app,
        source = window.source,
        mode = mode.as_str(),
        ocrChars = chars,
    );
    Ok(Some(captured_at))
}

fn write_text_record(sink: &Sink, meta: &Metadata, captured_at: DateTime<Utc>) -> Result<()> {
    match sink {
        Sink::Spool(spool) => spool.write_text(meta, captured_at),
        Sink::Local(store) => store.append(meta, captured_at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_webp_resizes_long_side() {
        let img = RgbImage::from_pixel(3200, 1600, image::Rgb([120, 130, 140]));
        let bytes = to_webp(&img).unwrap();
        // It is a WebP container (RIFF....WEBP)
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WEBP");
    }
}
