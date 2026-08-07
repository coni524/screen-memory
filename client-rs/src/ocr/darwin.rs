//! Local OCR through the macOS Vision framework.
//!
//! Builds a CGImage from the captured full-resolution RGB image and extracts text
//! with VNRecognizeTextRequest (accuracy-first, Japanese plus English). The image
//! never leaves the process.

use anyhow::{Context, Result};
use image::RgbImage;
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_core_foundation::{CFData, CFRetained};
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage,
};
use objc2_foundation::{NSArray, NSDictionary, NSString};
use objc2_vision::{
    VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
};

pub fn recognize(img: &RgbImage) -> Result<String> {
    let cg = make_cg_image(img)?;

    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setUsesLanguageCorrection(true);
    request.setRecognitionLanguages(&NSArray::from_retained_slice(&[
        NSString::from_str("ja-JP"),
        NSString::from_str("en-US"),
    ]));

    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            &cg,
            &NSDictionary::new(),
        )
    };
    let as_request: Retained<VNRequest> =
        Retained::into_super(Retained::into_super(request.clone()));
    handler
        .performRequests_error(&NSArray::from_retained_slice(&[as_request]))
        .map_err(|e| anyhow::anyhow!("OCR failed: {}", e.localizedDescription()))?;

    let mut text = String::new();
    for observation in request.results().iter().flatten() {
        if let Some(candidate) = observation.topCandidates(1).iter().next() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&candidate.string().to_string());
        }
    }
    Ok(text)
}

/// Builds a CGImage from a 24bpp RGB pixel buffer (the CFData copy is the only copy).
fn make_cg_image(img: &RgbImage) -> Result<CFRetained<CGImage>> {
    let width = img.width() as usize;
    let height = img.height() as usize;
    let raw = img.as_raw();
    let data = unsafe { CFData::new(None, raw.as_ptr(), raw.len() as isize) }
        .context("cannot create the CFData for the pixel data")?;
    let provider =
        CGDataProvider::with_cf_data(Some(&data)).context("cannot create the CGDataProvider")?;
    let space = CGColorSpace::new_device_rgb().context("cannot create the RGB color space")?;
    unsafe {
        CGImage::new(
            width,
            height,
            8,
            24,
            width * 3,
            Some(&space),
            CGBitmapInfo(0), // kCGImageAlphaNone
            Some(&provider),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .context("cannot create the CGImage")
}
