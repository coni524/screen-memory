//! Storage backend for local-only mode. Nothing goes to AWS; metadata carrying the
//! OCR text is appended to a JSONL file (one JSON object per line) per JST date.
//!
//! One file per day keeps the file count low, which suits the intended use of
//! analyzing the whole set later with Claude Code or a local LLM. Nothing is deleted
//! to enforce a cap: this is the user's own data.

use std::io::Write;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Asia::Tokyo;

use crate::config::state_dir;
use crate::spool::Metadata;

/// Storage directory. The "open storage folder" menu item opens this same path.
pub fn local_dir() -> PathBuf {
    state_dir().join("local")
}

pub struct LocalStore {
    dir: PathBuf,
}

impl LocalStore {
    pub fn new(dir: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&dir).with_context(|| {
            format!("cannot create the local data directory: {}", dir.display())
        })?;
        Ok(Self { dir })
    }

    /// Append a single record to the JSONL file for that day in JST.
    pub fn append(&self, metadata: &Metadata, captured_at: DateTime<Utc>) -> Result<()> {
        let day = captured_at.with_timezone(&Tokyo).format("%Y-%m-%d");
        let path = self.dir.join(format!("{day}.jsonl"));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .with_context(|| format!("cannot open the local data file: {}", path.display()))?;
        let mut line = serde_json::to_string(metadata)?;
        line.push('\n');
        file.write_all(line.as_bytes())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spool::build_text_metadata;
    use chrono::TimeZone;

    #[test]
    fn append_writes_one_json_per_line_in_jst_file() {
        let dir = std::env::temp_dir().join(format!("sm-local-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = LocalStore::new(dir.clone()).unwrap();
        // A UTC instant that falls on 2026-08-01 in JST (at or after 15:00 UTC the day before)
        let at = Utc.with_ymd_and_hms(2026, 7, 31, 16, 0, 0).unwrap();
        for text in ["1行目", "2行目"] {
            store
                .append(&build_text_metadata("d", at, "App", "T", text.into()), at)
                .unwrap();
        }
        let content = std::fs::read_to_string(dir.join("2026-08-01.jsonl")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["ocrText"], "1行目");
        assert_eq!(first["capturedAt"], "2026-08-01T01:00:00+09:00");
    }
}
