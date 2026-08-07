//! Windows capture backend.
//!
//! `GetForegroundWindow` identifies the foreground window, and `PrintWindow`
//! (`PW_RENDERFULLCONTENT`) captures that window alone.
//! Windows has no screen recording permission model, so preflight only performs
//! initialization.

use anyhow::{Context, Result, bail};
use image::RgbImage;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, GdiFlush, GetDC, HBITMAP, HDC, HGDIOBJ, ReleaseDC, SelectObject,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId,
    PW_RENDERFULLCONTENT,
};
use windows::core::PWSTR;

use super::{ActiveWindow, Capturer};

pub struct WindowsCapturer {
    /// Window handle from the preceding active_window() call, used by grab()
    last_hwnd: Option<HWND>,
}

impl WindowsCapturer {
    pub fn new() -> Self {
        WindowsCapturer { last_hwnd: None }
    }
}

impl Capturer for WindowsCapturer {
    /// Initializes DPI awareness and WinRT (used by local OCR) on this thread.
    /// Windows has no screen recording permission, so this always returns true.
    fn preflight(&mut self) -> bool {
        // Per-Monitor v2, applied process-wide. Failures such as setting it twice are safe to ignore
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        if let Err(e) = unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            tracing::warn!(event = "winrt_init_error", error = %e);
        }
        true
    }

    fn idle_seconds(&mut self) -> f64 {
        let mut info = LASTINPUTINFO {
            cbSize: size_of::<LASTINPUTINFO>() as u32,
            dwTime: 0,
        };
        if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
            return 0.0; // When it cannot be read, treat as non-idle and keep capturing
        }
        // Both are 32-bit millisecond counts since boot; wrapping_sub survives the 49.7-day wrap
        let elapsed = unsafe { GetTickCount() }.wrapping_sub(info.dwTime);
        f64::from(elapsed) / 1000.0
    }

    fn active_window(&mut self) -> Result<ActiveWindow> {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd.is_invalid() {
            bail!("there is no foreground window");
        }

        let mut buf = [0u16; 512];
        let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
        let title = String::from_utf16_lossy(&buf[..len.max(0) as usize]);

        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == 0 {
            bail!("cannot get the foreground window's process id");
        }
        let app = process_name(pid)?;

        self.last_hwnd = Some(hwnd);
        Ok(ActiveWindow {
            app,
            title,
            window_id: hwnd.0 as usize as u32,
            source: "foreground",
        })
    }

    fn grab(&mut self, _window: &ActiveWindow) -> Result<RgbImage> {
        let hwnd = self.last_hwnd.context("call active_window() first")?;
        unsafe { capture_hwnd(hwnd) }
    }
}

/// Returns the executable name (without extension) as the application name.
fn process_name(pid: u32) -> Result<String> {
    let handle: HANDLE = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
        .context("cannot open the process")?;
    let mut buf = [0u16; 1024];
    let mut size = buf.len() as u32;
    let result = unsafe {
        QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut size,
        )
    };
    let _ = unsafe { CloseHandle(handle) };
    result.context("cannot get the executable name")?;
    let path = String::from_utf16_lossy(&buf[..size as usize]);
    let stem = std::path::Path::new(&path)
        .file_stem()
        .and_then(|s| s.to_str())
        .context("cannot decode the executable name")?;
    Ok(stem.to_string())
}

/// Has PrintWindow render a single window as BGRA, and returns it as an RGB image.
unsafe fn capture_hwnd(hwnd: HWND) -> Result<RgbImage> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect) }.context("cannot get the window rect")?;
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width <= 0 || height <= 0 {
        bail!("the window rect is empty ({width}x{height})");
    }

    let screen_dc: HDC = unsafe { GetDC(None) };
    if screen_dc.is_invalid() {
        bail!("cannot get the screen device context");
    }
    let result = unsafe { print_to_bitmap(hwnd, screen_dc, width, height) };
    unsafe { ReleaseDC(None, screen_dc) };
    result
}

unsafe fn print_to_bitmap(hwnd: HWND, screen_dc: HDC, width: i32, height: i32) -> Result<RgbImage> {
    let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
    if mem_dc.is_invalid() {
        bail!("cannot create the memory device context");
    }

    // A negative biHeight yields a top-down bitmap (topmost row first)
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let bitmap: Result<HBITMAP, _> =
        unsafe { CreateDIBSection(Some(screen_dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) };
    let bitmap = match bitmap {
        Ok(b) => b,
        Err(e) => {
            let _ = unsafe { DeleteDC(mem_dc) };
            return Err(e).context("cannot create the bitmap");
        }
    };

    let old: HGDIOBJ = unsafe { SelectObject(mem_dc, bitmap.into()) };
    let ok = unsafe { PrintWindow(hwnd, mem_dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)) };
    let _ = unsafe { GdiFlush() };

    let image = if ok.as_bool() {
        let len = width as usize * height as usize * 4;
        let bgra = unsafe { std::slice::from_raw_parts(bits.cast::<u8>(), len) };
        Some(bgra_to_rgb(bgra, width as u32, height as u32))
    } else {
        None
    };

    unsafe { SelectObject(mem_dc, old) };
    let _ = unsafe { DeleteObject(bitmap.into()) };
    let _ = unsafe { DeleteDC(mem_dc) };
    image.context("PrintWindow failed")
}

fn bgra_to_rgb(bgra: &[u8], width: u32, height: u32) -> RgbImage {
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for px in bgra.chunks_exact(4) {
        rgb.extend_from_slice(&[px[2], px[1], px[0]]);
    }
    RgbImage::from_raw(width, height, rgb).expect("the buffer length is width*height*3")
}
