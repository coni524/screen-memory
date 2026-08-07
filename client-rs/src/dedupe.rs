//! Duplicate detection based on dHash (difference hash).

use image::RgbImage;
use image::imageops::FilterType;

/// Convert to grayscale, downscale to 9x8, then compare each pixel with its right
/// neighbor to build a 64-bit hash.
pub fn dhash(img: &RgbImage) -> u64 {
    let gray = image::imageops::grayscale(img);
    let small = image::imageops::resize(&gray, 9, 8, FilterType::Lanczos3);
    let mut bits = 0u64;
    for row in 0..8 {
        for col in 0..8 {
            let left = small.get_pixel(col, row).0[0];
            let right = small.get_pixel(col + 1, row).0[0];
            bits = (bits << 1) | u64::from(left > right);
        }
    }
    bits
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a striped image (vertical gradient).
    fn gradient_image(seed: u32) -> RgbImage {
        RgbImage::from_fn(160, 120, |x, y| {
            let v = ((x * 7 + y * 3 + seed * 41) % 256) as u8;
            image::Rgb([v, v, v])
        })
    }

    #[test]
    fn same_image_distance_zero() {
        let a = gradient_image(1);
        assert_eq!(hamming(dhash(&a), dhash(&a)), 0);
    }

    #[test]
    fn unrelated_images_distance_large() {
        // Unrelated patterns end up far apart in Hamming distance (well beyond the
        // threshold of 5).
        let a = RgbImage::from_fn(160, 120, |x, _| {
            image::Rgb([if x % 2 == 0 { 0 } else { 255 }; 3])
        });
        let b = RgbImage::from_fn(160, 120, |_, y| {
            image::Rgb([if (y / 15) % 2 == 0 { 0 } else { 255 }; 3])
        });
        assert!(hamming(dhash(&a), dhash(&b)) > 10);
    }
}
