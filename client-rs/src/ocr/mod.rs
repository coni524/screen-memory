//! Local OCR. Extracts text from the captured full-resolution image, keeping the
//! image inside the process. The backend differs per OS (the Vision framework on
//! macOS, Windows.Media.Ocr on Windows).

use anyhow::Result;
use image::RgbImage;

#[cfg(target_os = "macos")]
mod darwin;

#[cfg(target_os = "windows")]
mod windows;

/// Maximum number of characters of OCR text. Kept in sync with the limit the
/// analysis Lambda puts into its prompt.
pub const MAX_TEXT_CHARS: usize = 4000;

pub fn recognize(img: &RgbImage) -> Result<String> {
    #[cfg(target_os = "macos")]
    let text = darwin::recognize(img)?;
    #[cfg(target_os = "windows")]
    let text = windows::recognize(img)?;
    Ok(truncate_chars(text, MAX_TEXT_CHARS))
}

fn truncate_chars(mut s: String, max: usize) -> String {
    if let Some((idx, _)) = s.char_indices().nth(max) {
        s.truncate(idx);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate_chars("あいうえお".to_string(), 3), "あいう");
        assert_eq!(truncate_chars("ab".to_string(), 3), "ab");
    }

    /// Running a solid-color image with no text drawn on it: the only claim is that
    /// an empty result does not crash
    #[test]
    fn recognize_blank_image_is_empty() {
        let img = RgbImage::from_pixel(64, 64, image::Rgb([255, 255, 255]));
        let text = recognize(&img).unwrap();
        assert!(text.is_empty());
    }
}
