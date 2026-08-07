//! Local OCR through Windows.Media.Ocr (the WinRT OCR API).
//!
//! Converts the captured RGB image into a BGRA SoftwareBitmap and hands it to
//! OcrEngine. The engine is created for Japanese when the Japanese language pack
//! is installed, and from the user's language settings otherwise.
//! OcrEngine caps the length of each side (MaxImageDimension, around 2600 on real
//! machines), so larger images are scaled down to fit while preserving aspect ratio.

use anyhow::{Context, Result};
use image::RgbImage;
use image::imageops::FilterType;
use windows::Globalization::Language;
use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Security::Cryptography::CryptographicBuffer;
use windows::core::HSTRING;

pub fn recognize(img: &RgbImage) -> Result<String> {
    let engine = make_engine()?;
    let max = OcrEngine::MaxImageDimension().unwrap_or(2600);

    let resized;
    let img = if img.width().max(img.height()) > max {
        let scale = f64::from(max) / f64::from(img.width().max(img.height()));
        let w = ((f64::from(img.width()) * scale) as u32).max(1);
        let h = ((f64::from(img.height()) * scale) as u32).max(1);
        resized = image::imageops::resize(img, w, h, FilterType::Lanczos3);
        &resized
    } else {
        img
    };

    let bitmap = make_software_bitmap(img)?;
    let result = engine
        .RecognizeAsync(&bitmap)
        .context("cannot start OCR")?
        .join()
        .context("OCR failed")?;

    let mut text = String::new();
    for line in result.Lines().context("cannot read the OCR result")? {
        let line_text = line.Text().context("cannot read an OCR line")?.to_string();
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&collapse_cjk_spaces(&line_text));
    }
    Ok(text)
}

/// The goal is to read Japanese and English, so the Japanese engine comes first
/// (it reads Latin characters too). Without the Japanese language pack, fall back
/// to the user's language settings.
fn make_engine() -> Result<OcrEngine> {
    if let Ok(lang) = Language::CreateLanguage(&HSTRING::from("ja"))
        && let Ok(engine) = OcrEngine::TryCreateFromLanguage(&lang)
    {
        return Ok(engine);
    }
    OcrEngine::TryCreateFromUserProfileLanguages()
        .context("cannot create the OCR engine (the language pack is missing)")
}

fn make_software_bitmap(img: &RgbImage) -> Result<SoftwareBitmap> {
    let mut bgra = Vec::with_capacity(img.as_raw().len() / 3 * 4);
    for px in img.as_raw().chunks_exact(3) {
        bgra.extend_from_slice(&[px[2], px[1], px[0], 255]);
    }
    let buffer = CryptographicBuffer::CreateFromByteArray(&bgra)
        .context("cannot create the pixel buffer")?;
    SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        img.width() as i32,
        img.height() as i32,
    )
    .context("cannot create the SoftwareBitmap")
}

/// Windows OCR inserts ASCII spaces between words even in Japanese text.
/// A space whose nearest neighbours on both sides are non-ASCII is treated as
/// not being a separator, and is removed.
fn collapse_cjk_spaces(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == ' '
            && let Some(prev) = chars[..i].iter().rev().find(|&&p| p != ' ')
            && let Some(next) = chars[i + 1..].iter().find(|&&n| n != ' ')
            && !prev.is_ascii()
            && !next.is_ascii()
        {
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapse_removes_spaces_between_cjk_only() {
        assert_eq!(collapse_cjk_spaces("画 面 を 撮 る"), "画面を撮る");
        assert_eq!(collapse_cjk_spaces("Rust で OCR する"), "Rust で OCR する");
        assert_eq!(collapse_cjk_spaces("open source"), "open source");
    }
}
