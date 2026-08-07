//! On-disk staging (the spool) plus generation of S3 keys and metadata.

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Asia::Tokyo;
use serde::{Deserialize, Serialize};

pub const MAX_SPOOL_FILES: usize = 2000;

pub fn image_key(device_id: &str, captured_at: DateTime<Utc>) -> String {
    let dt = captured_at.with_timezone(&Tokyo);
    format!("raw/{device_id}/{}.webp", dt.format("%Y/%m/%d/%H%M%S"))
}

pub fn json_key(device_id: &str, captured_at: DateTime<Utc>) -> String {
    let dt = captured_at.with_timezone(&Tokyo);
    format!("raw/{device_id}/{}.json", dt.format("%Y/%m/%d/%H%M%S"))
}

/// Capture metadata JSON. The key names are a contract with the analysis Lambda that
/// reads them, so do not rename them. Image mode carries imageKey; local OCR mode
/// carries ocrText instead.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub device: String,
    pub captured_at: String,
    pub app: String,
    pub window_title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ocr_text: Option<String>,
}

pub fn build_metadata(
    device_id: &str,
    captured_at: DateTime<Utc>,
    app: &str,
    window_title: &str,
) -> Metadata {
    let dt = captured_at.with_timezone(&Tokyo);
    Metadata {
        device: device_id.to_string(),
        captured_at: dt.format("%Y-%m-%dT%H:%M:%S%:z").to_string(),
        app: app.to_string(),
        window_title: window_title.to_string(),
        image_key: Some(image_key(device_id, captured_at)),
        ocr_text: None,
    }
}

/// Metadata for local OCR mode. No image is involved, so imageKey is omitted.
pub fn build_text_metadata(
    device_id: &str,
    captured_at: DateTime<Utc>,
    app: &str,
    window_title: &str,
    ocr_text: String,
) -> Metadata {
    Metadata {
        image_key: None,
        ocr_text: Some(ocr_text),
        ..build_metadata(device_id, captured_at, app, window_title)
    }
}

/// Stages image and metadata JSON pairs on disk.
///
/// File names are `%Y%m%d-%H%M%S` in JST. The uploader discovers a pair by the presence
/// of the .json file, so the image is written first and the JSON is then put in place by
/// renaming a temporary file, never exposing a half-written pair.
pub struct Spool {
    dir: PathBuf,
    max_files: usize,
}

/// A single spool entry. The JSON is always present; the webp only in image mode.
pub struct Entry {
    pub json: PathBuf,
    pub webp: Option<PathBuf>,
}

impl Spool {
    pub fn new(dir: PathBuf, max_files: usize) -> Result<Self> {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create the spool directory: {}", dir.display()))?;
        Ok(Self { dir, max_files })
    }

    pub fn write(
        &self,
        image_bytes: &[u8],
        metadata: &Metadata,
        captured_at: DateTime<Utc>,
    ) -> Result<()> {
        let stamp = captured_at.with_timezone(&Tokyo).format("%Y%m%d-%H%M%S");
        std::fs::write(self.dir.join(format!("{stamp}.webp")), image_bytes)?;
        self.write_json(&stamp.to_string(), metadata)?;
        self.enforce_limit()?;
        Ok(())
    }

    /// For local OCR mode: stage only the JSON, without writing an image.
    pub fn write_text(&self, metadata: &Metadata, captured_at: DateTime<Utc>) -> Result<()> {
        let stamp = captured_at.with_timezone(&Tokyo).format("%Y%m%d-%H%M%S");
        self.write_json(&stamp.to_string(), metadata)?;
        self.enforce_limit()?;
        Ok(())
    }

    fn write_json(&self, stamp: &str, metadata: &Metadata) -> Result<()> {
        let tmp = self.dir.join(format!("{stamp}.json.tmp"));
        std::fs::write(&tmp, serde_json::to_string(metadata)?)?;
        std::fs::rename(&tmp, self.dir.join(format!("{stamp}.json")))?;
        Ok(())
    }

    /// Return the staged entries, oldest first. webp is Some only in image mode.
    pub fn entries(&self) -> Vec<Entry> {
        self.sorted_files("json")
            .into_iter()
            .map(|json| {
                let webp = json.with_extension("webp");
                Entry {
                    webp: webp.exists().then_some(webp),
                    json,
                }
            })
            .collect()
    }

    fn sorted_files(&self, ext: &str) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == ext))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    fn enforce_limit(&self) -> Result<()> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "webp" || e == "json"))
            .collect();
        files.sort();
        let excess = files.len().saturating_sub(self.max_files);
        for path in &files[..excess] {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn jst_noon() -> DateTime<Utc> {
        // 2026-08-01 10:23:00 JST is 01:23:00 UTC
        Utc.with_ymd_and_hms(2026, 8, 1, 1, 23, 0).unwrap()
    }

    #[test]
    fn image_key_is_jst() {
        assert_eq!(
            image_key("mac-main", jst_noon()),
            "raw/mac-main/2026/08/01/102300.webp"
        );
    }

    #[test]
    fn metadata_matches_conventions() {
        let meta = build_metadata("mac-main", jst_noon(), "Visual Studio Code", "DESIGN.md");
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&meta).unwrap()).unwrap();
        assert_eq!(value["device"], "mac-main");
        assert_eq!(value["capturedAt"], "2026-08-01T10:23:00+09:00");
        assert_eq!(value["app"], "Visual Studio Code");
        assert_eq!(value["windowTitle"], "DESIGN.md");
        assert_eq!(value["imageKey"], "raw/mac-main/2026/08/01/102300.webp");
        assert_eq!(value.as_object().unwrap().len(), 5);
    }

    #[test]
    fn text_metadata_has_ocr_text_and_no_image_key() {
        let meta = build_text_metadata("mac-main", jst_noon(), "App", "T", "画面の文字".into());
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&meta).unwrap()).unwrap();
        assert_eq!(value["ocrText"], "画面の文字");
        assert!(value.get("imageKey").is_none());
        assert_eq!(value.as_object().unwrap().len(), 5);
        assert_eq!(
            json_key("mac-main", jst_noon()),
            "raw/mac-main/2026/08/01/102300.json"
        );
    }

    fn temp_spool(max_files: usize, name: &str) -> Spool {
        let dir = std::env::temp_dir().join(format!("sm-spool-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Spool::new(dir, max_files).unwrap()
    }

    #[test]
    fn write_creates_pair_and_entries_are_ordered() {
        let spool = temp_spool(MAX_SPOOL_FILES, "pairs");
        for minute in [2u32, 1] {
            let at = Utc.with_ymd_and_hms(2026, 8, 1, 1, minute, 0).unwrap();
            spool
                .write(b"img", &build_metadata("d", at, "App", ""), at)
                .unwrap();
        }
        let entries = spool.entries();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.webp.is_some()));
        // Oldest first
        assert!(entries[0].json < entries[1].json);
    }

    #[test]
    fn write_text_creates_json_only_entry() {
        let spool = temp_spool(MAX_SPOOL_FILES, "text");
        let at = jst_noon();
        spool
            .write_text(&build_text_metadata("d", at, "App", "", "text".into()), at)
            .unwrap();
        let entries = spool.entries();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].webp.is_none());
    }

    #[test]
    fn limit_deletes_oldest() {
        // With a cap of 4 files (2 pairs), writing 3 pairs drops both the webp and the
        // json of the oldest pair
        let spool = temp_spool(4, "limit");
        for minute in [1u32, 2, 3] {
            let at = Utc.with_ymd_and_hms(2026, 8, 1, 1, minute, 0).unwrap();
            spool
                .write(b"img", &build_metadata("d", at, "App", ""), at)
                .unwrap();
        }
        let entries = spool.entries();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().all(|e| e.webp.is_some()));
    }
}
