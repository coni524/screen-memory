//! The tray artwork, shared by both platforms.
//!
//! Everything is drawn procedurally in a 32x32 coordinate grid, so the same shapes render at
//! whatever pixel size each platform wants. Two renderings exist:
//!
//! - `render_glyph`: the shape alone in one color on transparent (the Windows notification area,
//!   where a colored plate would fight with the taskbar).
//! - `render_badge`: a white shape on the green rounded square, which is the same artwork as the
//!   app icon (packaging/make-icns.sh draws it in Swift from these very numbers).

/// A function saying whether a point lies inside the shape. One of these defines a whole icon.
pub type Ink = fn(f32, f32) -> bool;

/// The side of the coordinate grid the shapes are drawn in.
const GRID: f32 = 32.0;
/// A mid brightness that stays visible against both light and dark taskbars.
pub const ACTIVE_COLOR: [u8; 3] = [46, 160, 67];
pub const PAUSED_COLOR: [u8; 3] = [140, 140, 148];
/// The corner radius of the badge plate (the Big Sur and later app icon shape).
const BADGE_RADIUS: f32 = 7.2;
/// How far the shape shrinks inside the badge plate, so the plate reads as a background rather
/// than as a frame the shape is bursting out of.
const GLYPH_SCALE: f32 = 0.75;
/// The point the shrink happens around: the center of the camera shape.
const GLYPH_CENTER: (f32, f32) = (16.0, 16.5);

/// Draw the shape in one color on a transparent background. Only Windows shows the shape this
/// way, but the tests exercise it on either platform.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn render_glyph(size: u32, color: [u8; 3], ink: Ink) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for_each_pixel(size, |x, y| {
        let mut hits = 0;
        for_each_sample(size, x, y, |fx, fy| {
            if ink(fx, fy) {
                hits += 1;
            }
        });
        let alpha = (255 * hits / SUB_TOTAL) as u8;
        rgba.extend_from_slice(&[color[0], color[1], color[2], alpha]);
    });
    rgba
}

/// Draw the shape in white on a rounded plate of `plate`, matching the app icon.
pub fn render_badge(size: u32, plate: [u8; 3], ink: Ink) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for_each_pixel(size, |x, y| {
        let mut on_plate = 0;
        let mut on_shape = 0;
        for_each_sample(size, x, y, |fx, fy| {
            if !badge_ink(fx, fy) {
                return;
            }
            on_plate += 1;
            // Shrink the shape around its own center, keeping it inside the plate
            let gx = GLYPH_CENTER.0 + (fx - GRID / 2.0) / GLYPH_SCALE;
            let gy = GLYPH_CENTER.1 + (fy - GRID / 2.0) / GLYPH_SCALE;
            if ink(gx, gy) {
                on_shape += 1;
            }
        });
        if on_plate == 0 {
            rgba.extend_from_slice(&[plate[0], plate[1], plate[2], 0]);
            return;
        }
        // Average white over plate within the covered part, and let the plate carry the alpha
        let mix = |c: u8| {
            ((255 * on_shape + u32::from(c) * (on_plate - on_shape)) / on_plate).min(255) as u8
        };
        let alpha = (255 * on_plate / SUB_TOTAL) as u8;
        rgba.extend_from_slice(&[mix(plate[0]), mix(plate[1]), mix(plate[2]), alpha]);
    });
    rgba
}

/// How many samples per pixel each axis gets. The icon is shown scaled down, so the edges need
/// smoothing.
const SUB: u32 = 4;
const SUB_TOTAL: u32 = SUB * SUB;

fn for_each_pixel(size: u32, mut f: impl FnMut(u32, u32)) {
    for y in 0..size {
        for x in 0..size {
            f(x, y);
        }
    }
}

/// Walk the supersample points of one pixel, in grid coordinates.
fn for_each_sample(size: u32, x: u32, y: u32, mut f: impl FnMut(f32, f32)) {
    let unit = GRID / size as f32;
    for sy in 0..SUB {
        for sx in 0..SUB {
            let fx = (x as f32 + (sx as f32 + 0.5) / SUB as f32) * unit;
            let fy = (y as f32 + (sy as f32 + 0.5) / SUB as f32) * unit;
            f(fx, fy);
        }
    }
}

/// The plate the badge shapes sit on: the full grid, rounded off.
fn badge_ink(x: f32, y: f32) -> bool {
    rounded_rect(x, y, 0.0, 0.0, GRID, GRID, BADGE_RADIUS)
}

/// The camera shape: fill the rounded-rectangle body plus the bump on top, and knock the lens
/// out as a ring.
pub fn camera_ink(x: f32, y: f32) -> bool {
    let body = rounded_rect(x, y, 2.0, 10.0, 30.0, 28.0, 3.0);
    let finder = rounded_rect(x, y, 8.0, 5.0, 16.0, 11.0, 1.5);
    let lens = ((x - 16.0).powi(2) + (y - 19.5).powi(2)).sqrt();
    // Keep the knocked-out ring 2 pixels wide so it survives the shrink to 16x16
    (body || finder) && !(4.2..=6.2).contains(&lens)
}

/// The pause shape: two vertical bars.
pub fn pause_ink(x: f32, y: f32) -> bool {
    rounded_rect(x, y, 6.0, 5.0, 13.0, 27.0, 1.5) || rounded_rect(x, y, 19.0, 5.0, 26.0, 27.0, 1.5)
}

fn rounded_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    // For the rounded corners, test only how far the point runs past the corner's center
    let dx = (x0 + r - x).max(x - (x1 - r)).max(0.0);
    let dy = (y0 + r - y).max(y - (y1 - r)).max(0.0);
    dx * dx + dy * dy <= r * r
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPES: [Ink; 2] = [camera_ink, pause_ink];

    #[test]
    fn glyphs_cover_pixels_without_filling_the_corner() {
        for ink in SHAPES {
            let rgba = render_glyph(32, ACTIVE_COLOR, ink);
            assert_eq!(rgba.len(), 32 * 32 * 4);
            assert_eq!(
                rgba[3], 0,
                "the top-left corner is outside the shape, so it is transparent"
            );
            assert!(
                rgba.chunks(4).any(|p| p[3] == 255),
                "there is at least one filled pixel"
            );
        }
    }

    #[test]
    fn badges_fill_the_plate_and_keep_the_shape_inside_it() {
        for ink in SHAPES {
            let rgba = render_badge(64, ACTIVE_COLOR, ink);
            assert_eq!(rgba.len(), 64 * 64 * 4);
            assert_eq!(rgba[3], 0, "the rounded corner is transparent");
            let center = ((32 * 64 + 32) * 4) as usize;
            let px = &rgba[center..center + 4];
            assert_eq!(px[3], 255, "the middle of the plate is opaque");
            assert!(
                rgba.chunks(4).any(|p| p[..3] == [255, 255, 255]),
                "the shape shows as white on the plate"
            );
            // With the shape shrunk, the plate color must still survive along its edges
            let edge = ((32 * 64 + 2) * 4) as usize;
            assert_eq!(
                rgba[edge..edge + 3],
                ACTIVE_COLOR,
                "the plate is visible around the shape"
            );
        }
    }
}
