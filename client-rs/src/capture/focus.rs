//! Identifies the window macOS considers focused.
//!
//! Focus on macOS has two levels: at the application level it is NSWorkspace's
//! frontmost application, and at the window level it is AXFocusedWindow from the
//! Accessibility API. AXFocusedWindow is the OS's own answer, so it is preferred,
//! but it requires the Accessibility permission. Where that permission is missing,
//! the front-to-back order from CGWindowList serves as a substitute. That order is
//! not focus as such, but for ordinary applications the two nearly always agree.

use std::ffi::c_void;
use std::ptr::NonNull;

use objc2_application_services::{AXError, AXIsProcessTrusted, AXUIElement, AXValue, AXValueType};
use objc2_core_foundation::{
    CFDictionary, CFNumber, CFRetained, CFString, CFType, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption,
    kCGWindowBounds, kCGWindowLayer, kCGWindowNumber, kCGWindowOwnerPID,
};

/// Tolerance (in points) when matching the rect from AX against the one from CGWindowList.
const FRAME_TOLERANCE: f64 = 2.0;

/// kCGNullWindowID (no relative window specified).
const NULL_WINDOW_ID: u32 = 0;

pub struct FocusedWindow {
    /// CGWindowID, used to look up the SCWindow
    pub id: u32,
    /// How it was determined: "ax" is the OS's own focus, "order" is the front-to-back fallback
    pub source: &'static str,
}

/// Whether this process holds the Accessibility permission.
pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Returns the focused window of the application with the given pid.
pub fn focused_window(pid: i32) -> Option<FocusedWindow> {
    let windows = ordered_windows(pid);
    let (front_id, _) = *windows.first()?;
    if let Some(frame) = ax_focused_frame(pid)
        && let Some((id, _)) = windows
            .iter()
            .find(|(_, bounds)| frames_match(*bounds, frame))
    {
        return Some(FocusedWindow {
            id: *id,
            source: "ax",
        });
    }
    Some(FocusedWindow {
        id: front_id,
        source: "order",
    })
}

/// Returns the ordinary windows (layer 0) of the given pid, front to back.
/// kCGWindowListOptionOnScreenOnly is documented to return windows in front-to-back order.
fn ordered_windows(pid: i32) -> Vec<(u32, CGRect)> {
    let Some(list) = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        NULL_WINDOW_ID,
    ) else {
        return Vec::new();
    };
    let mut windows = Vec::new();
    for index in 0..list.count() {
        let value = unsafe { list.value_at_index(index) };
        if value.is_null() {
            continue;
        }
        // Each element of CGWindowListCopyWindowInfo is a dictionary of window info
        let info: &CFDictionary = unsafe { &*value.cast() };
        if dict_i64(info, unsafe { kCGWindowOwnerPID }) != Some(i64::from(pid))
            || dict_i64(info, unsafe { kCGWindowLayer }) != Some(0)
        {
            continue;
        }
        let (Some(id), Some(bounds)) = (
            dict_i64(info, unsafe { kCGWindowNumber }),
            dict_rect(info, unsafe { kCGWindowBounds }),
        ) else {
            continue;
        };
        windows.push((id as u32, bounds));
    }
    windows
}

/// Rect of the focused window as reported by AX. None without permission, or for
/// applications that do not support it.
fn ax_focused_frame(pid: i32) -> Option<CGRect> {
    if !accessibility_trusted() {
        return None;
    }
    let app = unsafe { AXUIElement::new_application(pid) };
    let window = copy_attribute(&app, "AXFocusedWindow")?;
    let window: &AXUIElement = window.downcast_ref()?;
    let position = copy_attribute(window, "AXPosition")?;
    let size = copy_attribute(window, "AXSize")?;
    Some(CGRect::new(ax_point(&position)?, ax_size(&size)?))
}

fn copy_attribute(element: &AXUIElement, name: &str) -> Option<CFRetained<CFType>> {
    let name = CFString::from_str(name);
    let mut value: *const CFType = std::ptr::null();
    let error = unsafe { element.copy_attribute_value(&name, NonNull::from(&mut value)) };
    if error != AXError::Success {
        return None;
    }
    // The reference came from a copy_ call, so hand ownership over to CFRetained
    Some(unsafe { CFRetained::from_raw(NonNull::new(value.cast_mut())?) })
}

fn ax_point(value: &CFType) -> Option<CGPoint> {
    let value: &AXValue = value.downcast_ref()?;
    let mut point = CGPoint::new(0.0, 0.0);
    let out = NonNull::from(&mut point).cast::<c_void>();
    unsafe { value.value(AXValueType::CGPoint, out) }.then_some(point)
}

fn ax_size(value: &CFType) -> Option<CGSize> {
    let value: &AXValue = value.downcast_ref()?;
    let mut size = CGSize::new(0.0, 0.0);
    let out = NonNull::from(&mut size).cast::<c_void>();
    unsafe { value.value(AXValueType::CGSize, out) }.then_some(size)
}

fn dict_value(dict: &CFDictionary, key: &CFString) -> *const c_void {
    unsafe { dict.value((key as *const CFString).cast()) }
}

fn dict_i64(dict: &CFDictionary, key: &CFString) -> Option<i64> {
    let value = dict_value(dict, key);
    if value.is_null() {
        return None;
    }
    unsafe { &*value.cast::<CFNumber>() }.as_i64()
}

fn dict_rect(dict: &CFDictionary, key: &CFString) -> Option<CGRect> {
    let value = dict_value(dict, key);
    if value.is_null() {
        return None;
    }
    let bounds: &CFDictionary = unsafe { &*value.cast() };
    let mut rect = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(0.0, 0.0));
    unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &mut rect) }.then_some(rect)
}

/// AX and CGWindowList both use top-left-origin point coordinates, so the rects
/// can be compared directly.
fn frames_match(a: CGRect, b: CGRect) -> bool {
    (a.origin.x - b.origin.x).abs() <= FRAME_TOLERANCE
        && (a.origin.y - b.origin.y).abs() <= FRAME_TOLERANCE
        && (a.size.width - b.size.width).abs() <= FRAME_TOLERANCE
        && (a.size.height - b.size.height).abs() <= FRAME_TOLERANCE
}

#[cfg(test)]
mod tests {
    use super::*;

    // The coordinates mimic real values from a multi-display layout
    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
    }

    #[test]
    fn frames_match_allows_small_gap() {
        assert!(frames_match(
            rect(2068.0, 422.0, 1771.0, 1436.0),
            rect(2069.0, 422.0, 1771.0, 1435.0)
        ));
    }

    #[test]
    fn frames_match_rejects_other_window() {
        assert!(!frames_match(
            rect(2068.0, 422.0, 1771.0, 1436.0),
            rect(3840.0, 30.0, 1438.0, 1949.0)
        ));
    }
}
