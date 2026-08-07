//! The native Windows settings window, written straight against the Win32 API.
//!
//! The look follows the Windows 11 design language: a scrolling page of rounded "cards" grouped
//! under section headings, the system UI font at the WinUI type ramp, 32px controls on an 8px
//! grid, and light / dark following the OS app theme. Visual styles come from the application
//! manifest embedded by build.rs — without it every control would fall back to classic rendering.
//!
//! The window is split in two:
//!
//! - The frame window owns the fixed footer (the two buttons on a card-coloured bar).
//! - A child "content" window owns everything that scrolls: the input boxes, the language combo,
//!   and the result panel. It also paints the cards, headings, labels and hints itself, because
//!   STATIC controls cannot follow a card background and the WinUI type ramp at the same time.
//!
//! `layout_content()` is the single source of truth for both: it produces a display list for
//! WM_PAINT and a placement list for the real controls, in content coordinates. Scrolling then
//! only has to shift both by `scroll_y`.
//!
//! The message pump in tray/windows.rs runs on this same thread, so tab navigation and Enter /
//! Esc need dialog semantics, which is why the pump calls `is_dialog_message()` for every
//! message. The content window carries WS_EX_CONTROLPARENT so that traversal descends into it.
//! The process is Per-Monitor DPI aware, so all dimensions scale with the dpi and a move between
//! monitors (WM_DPICHANGED) rebuilds the layout.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, BitBlt, CLEARTYPE_QUALITY,
    CLIP_DEFAULT_PRECIS, CreateBitmap, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateDIBSection, CreateFontIndirectW, CreatePen, CreateSolidBrush, DEFAULT_CHARSET,
    DIB_RGB_COLORS, DRAW_TEXT_FORMAT, DT_END_ELLIPSIS, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER,
    DT_WORDBREAK, DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, GetMonitorInfoW,
    COLOR_HIGHLIGHT, GetSysColor, GetTextMetricsW, TEXTMETRICW,
    HBRUSH, HDC, HFONT, InvalidateRect, LOGFONTW, MONITOR_DEFAULTTONEAREST,
    MONITORINFO, MonitorFromWindow, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID,
    ReleaseDC, RoundRect, SRCCOPY, SelectObject, SetBkColor, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows::Win32::UI::Controls::{
    COMBOBOXINFO, EM_SETMARGINS, GetComboBoxInfo, SetScrollInfo, SetWindowTheme,
};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SystemParametersInfoForDpi,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_SETCURSEL, CB_SETITEMHEIGHT, CBN_SETFOCUS,
    CBS_DROPDOWNLIST,
    CREATESTRUCTW, CW_USEDEFAULT, CreateIconIndirect, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyWindow, EC_LEFTMARGIN, EC_RIGHTMARGIN, EN_KILLFOCUS, EN_SETFOCUS,
    ES_AUTOHSCROLL, GWLP_USERDATA, GetClientRect,
    GetScrollInfo, GetWindowLongPtrW, GetWindowTextLengthW, GetWindowTextW, HICON, HMENU,
    ICON_BIG, ICON_SMALL, ICONINFO, IDC_ARROW, IsDialogMessageW, IsWindow, LoadCursorW, MINMAXINFO,
    MSG, MoveWindow, NONCLIENTMETRICSW, RegisterClassExW, SB_VERT, SIF_ALL, SIF_PAGE, SIF_POS,
    SIF_RANGE, SM_CXVSCROLL, SPI_GETNONCLIENTMETRICS, SW_SHOW, SWP_NOACTIVATE, SWP_NOZORDER,
    SCROLLINFO, SendMessageW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CTLCOLORBTN,
    WM_CTLCOLOREDIT, WM_CTLCOLORLISTBOX, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED,
    WM_ERASEBKGND, WM_GETMINMAXINFO, WM_MOUSEWHEEL, WM_NCCREATE, WM_PAINT, WM_SETFONT, WM_SETICON,
    WM_SETTINGCHANGE, WM_SIZE, WM_THEMECHANGED, WM_VSCROLL, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_EX_CONTROLPARENT, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_TABSTOP,
    WS_THICKFRAME,
    WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

use super::{
    Step, broken_note, current_values, fields, intro, language_index, language_label,
    language_options, save, save_language, saved_message, spawn_test, testing_message,
    window_title,
};
use crate::config::default_config_path;
use crate::i18n::t;
use crate::tray::art;

const ID_TEST: usize = 101;
const ID_SAVE: usize = 102;
/// The well-known IDs IsDialogMessageW sends for Enter / Esc.
const ID_ENTER: usize = 1; // IDOK
const ID_ESC: usize = 2; // IDCANCEL

/// WM_VSCROLL request codes. Spelling them out avoids juggling the typed constants for the two
/// values we actually care about plus thumb dragging.
const SB_LINEUP: u32 = 0;
const SB_LINEDOWN: u32 = 1;
const SB_PAGEUP: u32 = 2;
const SB_PAGEDOWN: u32 = 3;
const SB_THUMBPOSITION: u32 = 4;
const SB_THUMBTRACK: u32 = 5;

// Layout dimensions, given in device-independent pixels at 96dpi and scaled by the actual dpi
// at layout time. The values follow the Windows 11 / WinUI metrics: an 8px grid, 32px controls,
// 24px page margins, 16px card padding.
const PAGE_W: i32 = 600;
const MAX_H: i32 = 720;
const MIN_W: i32 = 460;
const MIN_H: i32 = 400;
const PAGE_PAD: i32 = 24;
const CARD_PAD: i32 = 16;
const CARD_RADIUS: i32 = 8;
const SECTION_GAP: i32 = 24;
const CTRL_H: i32 = 32;
/// Left and right inset for the text inside an input box, matching a Windows 11 text box.
const EDIT_INSET: i32 = 10;
const BUTTON_W: i32 = 132;
const BUTTON_H: i32 = 32;
const BUTTON_GAP: i32 = 8;
const FOOTER_PAD: i32 = 16;
const COMBO_W: i32 = 220;
/// Frame a combo box adds around its selection field, subtracted so the closed control ends up
/// the same height as an input box. Measured against the themed control on Windows 11.
const COMBO_CHROME: i32 = 6;
/// Total height handed to the combo box; everything past the closed control is dropdown area.
const COMBO_DROP_H: i32 = 180;
/// How far one wheel notch / arrow click scrolls.
const SCROLL_STEP: i32 = 48;

/// HWND of the open settings window, shared with the pump's is_dialog_message().
static DIALOG: AtomicIsize = AtomicIsize::new(0);

/// Called by the tray's message pump for every message. While the settings window is open, it
/// gives Tab navigation and Enter / Esc their dialog behavior.
pub fn is_dialog_message(msg: &MSG) -> bool {
    let raw = DIALOG.load(Ordering::Relaxed);
    if raw == 0 {
        return false;
    }
    unsafe { IsDialogMessageW(HWND(raw as *mut _), msg).as_bool() }
}

// ---------------------------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------------------------

/// 0x00BBGGRR, the order COLORREF wants.
const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF((r as u32) | ((g as u32) << 8) | ((b as u32) << 16))
}

/// The palette, picked to match the Windows 11 common controls in either mode.
#[derive(Clone, Copy)]
struct Theme {
    dark: bool,
    /// Page background, behind the cards.
    page: COLORREF,
    /// Card fill, and the footer bar.
    card: COLORREF,
    /// Card outline and the row dividers.
    border: COLORREF,
    text: COLORREF,
    /// Hints, the config path — WinUI's "text secondary".
    sub: COLORREF,
    /// Input box background, handed back from WM_CTLCOLOREDIT.
    field: COLORREF,
    ok: COLORREF,
    ng: COLORREF,
}

impl Theme {
    fn load() -> Theme {
        if app_uses_dark_mode() {
            Theme {
                dark: true,
                page: rgb(0x20, 0x20, 0x20),
                card: rgb(0x2B, 0x2B, 0x2B),
                border: rgb(0x38, 0x38, 0x38),
                text: rgb(0xFF, 0xFF, 0xFF),
                sub: rgb(0xC8, 0xC8, 0xC8),
                field: rgb(0x2F, 0x2F, 0x2F),
                ok: rgb(0x6C, 0xCB, 0x5F),
                ng: rgb(0xFF, 0x99, 0xA4),
            }
        } else {
            Theme {
                dark: false,
                page: rgb(0xF3, 0xF3, 0xF3),
                card: rgb(0xFB, 0xFB, 0xFB),
                border: rgb(0xE5, 0xE5, 0xE5),
                text: rgb(0x1B, 0x1B, 0x1B),
                sub: rgb(0x5E, 0x5E, 0x5E),
                field: rgb(0xFF, 0xFF, 0xFF),
                ok: rgb(0x0F, 0x7B, 0x0F),
                ng: rgb(0xC4, 0x2B, 0x1C),
            }
        }
    }

    fn tone(&self, tone: Tone) -> COLORREF {
        match tone {
            Tone::Text => self.text,
            Tone::Sub => self.sub,
            Tone::Ok => self.ok,
            Tone::Ng => self.ng,
        }
    }
}

/// The app (not system) theme preference. Absent or non-zero means light, which is also the
/// right answer on the Windows versions that predate the value.
///
/// `SCREEN_MEMORY_THEME=dark|light` forces the choice, mirroring the `SCREEN_MEMORY_LANG` escape
/// hatch in i18n.rs: it makes either appearance checkable without touching the OS setting.
fn app_uses_dark_mode() -> bool {
    match std::env::var("SCREEN_MEMORY_THEME").as_deref() {
        Ok("dark") => return true,
        Ok("light") => return false,
        _ => {}
    }
    let mut value: u32 = 1;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut _),
            Some(&mut size),
        )
    };
    status.is_ok() && value == 0
}

// ---------------------------------------------------------------------------------------------
// Fonts
// ---------------------------------------------------------------------------------------------

/// The WinUI type ramp, in the face the OS uses for UI text (Segoe UI Variable on an English
/// Windows 11, Yu Gothic UI on a Japanese one), so the window matches the rest of the shell.
#[derive(Clone, Copy)]
struct Fonts {
    /// Section headings, semibold.
    heading: HFONT,
    /// Row labels and body text.
    label: HFONT,
    /// Hints and the config path.
    caption: HFONT,
    /// Buttons and the combo box.
    body: HFONT,
    /// The input boxes: IDs and ARNs read better in a fixed-pitch face.
    mono: HFONT,
}

impl Fonts {
    unsafe fn build(dpi: u32) -> Fonts {
        // lfMessageFont is the shell's own UI font for the current locale, already at this dpi
        let mut metrics = NONCLIENTMETRICSW {
            cbSize: size_of::<NONCLIENTMETRICSW>() as u32,
            ..Default::default()
        };
        let base = unsafe {
            SystemParametersInfoForDpi(
                SPI_GETNONCLIENTMETRICS.0,
                size_of::<NONCLIENTMETRICSW>() as u32,
                Some(&mut metrics as *mut _ as *mut _),
                0,
                dpi,
            )
        }
        .is_ok()
        .then_some(metrics.lfMessageFont)
        .unwrap_or_default();

        let px = |dip: i32| -(dip * dpi as i32 / 96);
        let make = |height: i32, weight: i32, face: Option<PCWSTR>| -> HFONT {
            let mut lf = LOGFONTW {
                lfHeight: height,
                lfWeight: weight,
                lfCharSet: DEFAULT_CHARSET,
                lfOutPrecision: OUT_DEFAULT_PRECIS,
                lfClipPrecision: CLIP_DEFAULT_PRECIS,
                lfQuality: CLEARTYPE_QUALITY,
                ..base
            };
            if let Some(face) = face {
                lf.lfFaceName = [0; 32];
                let name = unsafe { face.as_wide() };
                let n = name.len().min(31);
                lf.lfFaceName[..n].copy_from_slice(&name[..n]);
            }
            unsafe { CreateFontIndirectW(&lf) }
        };
        Fonts {
            heading: make(px(15), 600, None),
            label: make(px(14), 400, None),
            caption: make(px(12), 400, None),
            body: make(px(14), 400, None),
            mono: make(px(13), 400, Some(w!("Consolas"))),
        }
    }

    fn get(&self, face: Face) -> HFONT {
        match face {
            Face::Heading => self.heading,
            Face::Label => self.label,
            Face::Caption => self.caption,
        }
    }

    unsafe fn delete(&self) {
        for f in [self.heading, self.label, self.caption, self.body, self.mono] {
            if !f.is_invalid() {
                let _ = unsafe { DeleteObject(f.into()) };
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Display list
// ---------------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Face {
    Heading,
    Label,
    Caption,
}

impl Face {
    /// Index into TextPen's per-face metrics.
    fn index(self) -> usize {
        match self {
            Face::Heading => 0,
            Face::Label => 1,
            Face::Caption => 2,
        }
    }
}

#[derive(Clone, Copy)]
enum Tone {
    Text,
    Sub,
    Ok,
    Ng,
}

/// One item painted by the content window. Colours resolve at paint time, so a theme switch is
/// just an InvalidateRect — no relayout.
enum Draw {
    Card(RECT),
    Divider(RECT),
    /// The box drawn around an input. A single-line EDIT always puts its text at the top of its
    /// client area, so a 32px-tall bordered EDIT would leave all the slack underneath the text.
    /// Instead the EDIT is borderless and only as tall as one line, and this is the Windows 11
    /// text box painted around it. `hwnd` identifies the input so the focus ring can be drawn.
    Field {
        rect: RECT,
        hwnd: HWND,
    },
    Text {
        rect: RECT,
        text: String,
        face: Face,
        tone: Tone,
        flags: DRAW_TEXT_FORMAT,
    },
}

/// Where a real child control sits, in content coordinates.
struct Placement {
    hwnd: HWND,
    rect: RECT,
    /// Extra height handed to a combo box for its dropdown; 0 for everything else.
    drop_h: i32,
}

/// What the result panel shows.
enum Notice {
    None,
    /// A one-liner: the "running…" placeholder, a save confirmation, or a save error.
    Line { ok: bool, text: String },
    /// The connection check's per-step outcome.
    Steps(Vec<Step>),
}

/// The fields, grouped into the cards they are shown in.
fn sections() -> [(&'static str, &'static [&'static str]); 2] {
    [
        (t("This device", "この端末"), &["device_id"]),
        (
            t("AWS connection", "AWS 接続"),
            &[
                "bucket",
                "region",
                "user_pool_id",
                "user_pool_client_id",
                "identity_pool_id",
                "cognito_domain",
            ],
        ),
    ]
}

// ---------------------------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------------------------

/// The screen state, touched by both wndprocs and Window. It is boxed when the window is
/// created, and the raw pointer is held in the frame's GWLP_USERDATA, in the content window's
/// GWLP_USERDATA, and in Window (all on the same thread). Only Window's Drop frees it.
struct State {
    saved_tx: mpsc::Sender<()>,
    /// Some only while a connection check is running.
    test_rx: Option<mpsc::Receiver<Vec<Step>>>,
    content: HWND,
    /// The input fields, in `fields()` order.
    inputs: Vec<(&'static str, HWND)>,
    lang_combo: HWND,
    test_button: HWND,
    save_button: HWND,
    icon: HICON,
    /// The input that currently has focus, so its box can be drawn with the focus ring.
    focus: HWND,
    /// config.toml could not be read, so the form is empty and saving is refused.
    broken: bool,
    notice: Notice,
    theme: Theme,
    fonts: Fonts,
    page_brush: HBRUSH,
    card_brush: HBRUSH,
    field_brush: HBRUSH,
    dpi: u32,
    /// Content scroll offset, always within 0..=(content_h - viewport_h).
    scroll_y: i32,
    content_h: i32,
    draws: Vec<Draw>,
    places: Vec<Placement>,
}

impl State {
    fn values(&self) -> super::Values {
        self.inputs
            .iter()
            .map(|(key, hwnd)| (*key, get_text(*hwnd)))
            .collect()
    }

    fn scale(&self, v: i32) -> i32 {
        v * self.dpi as i32 / 96
    }

    unsafe fn rebuild_brushes(&mut self) {
        for old in [self.page_brush, self.card_brush, self.field_brush] {
            if !old.is_invalid() {
                let _ = unsafe { DeleteObject(old.into()) };
            }
        }
        self.page_brush = unsafe { CreateSolidBrush(self.theme.page) };
        self.card_brush = unsafe { CreateSolidBrush(self.theme.card) };
        self.field_brush = unsafe { CreateSolidBrush(self.theme.field) };
    }
}

pub struct Window {
    hwnd: HWND,
    state: *mut State,
}

impl Window {
    /// Build the window and bring it to the front.
    pub fn open(saved_tx: mpsc::Sender<()>) -> Result<Window> {
        let instance: HINSTANCE = unsafe { module_handle() }?;
        register_classes(instance)?;
        let (values, broken) = current_values();
        let state = Box::into_raw(Box::new(State {
            saved_tx,
            test_rx: None,
            content: HWND::default(),
            inputs: Vec::new(),
            lang_combo: HWND::default(),
            test_button: HWND::default(),
            save_button: HWND::default(),
            icon: HICON::default(),
            broken,
            focus: HWND::default(),
            notice: Notice::None,
            theme: Theme::load(),
            fonts: Fonts {
                heading: HFONT::default(),
                label: HFONT::default(),
                caption: HFONT::default(),
                body: HFONT::default(),
                mono: HFONT::default(),
            },
            page_brush: HBRUSH::default(),
            card_brush: HBRUSH::default(),
            field_brush: HBRUSH::default(),
            dpi: 96,
            scroll_y: 0,
            content_h: 0,
            draws: Vec::new(),
            places: Vec::new(),
        }));
        let title = wide(window_title());
        let created = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                frame_class(),
                PCWSTR(title.as_ptr()),
                frame_style(),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(instance),
                Some(state.cast()),
            )
        };
        let hwnd = match created {
            Ok(hwnd) => hwnd,
            Err(e) => {
                drop(unsafe { Box::from_raw(state) });
                return Err(e).context("cannot create the settings window");
            }
        };
        unsafe {
            let st = &mut *state;
            st.dpi = GetDpiForWindow(hwnd);
            st.fonts = Fonts::build(st.dpi);
            st.rebuild_brushes();
            build_children(hwnd, st, &values, instance);
            apply_theme(hwnd, st);
            set_window_icon(hwnd, st);
            relayout(hwnd, st);
            size_to_content(hwnd, st);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            if let Some((_, first)) = st.inputs.first() {
                let _ = SetFocus(Some(*first));
            }
        }
        DIALOG.store(hwnd.0 as isize, Ordering::Relaxed);
        Ok(Window { hwnd, state })
    }

    /// Bring an already-open window to the front.
    pub fn focus(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    /// Becomes false once the close button is used; the tray loop then drops the Window.
    pub fn is_open(&self) -> bool {
        unsafe { IsWindow(Some(self.hwnd)) }.as_bool()
    }

    /// Show the connection check result if one has arrived. The tray loop calls this every turn.
    pub fn poll(&mut self) {
        if !self.is_open() {
            return;
        }
        let state = unsafe { &mut *self.state };
        let Some(rx) = &state.test_rx else { return };
        let outcome = match rx.try_recv() {
            Err(mpsc::TryRecvError::Empty) => return,
            Ok(steps) => Ok(steps),
            Err(mpsc::TryRecvError::Disconnected) => Err(()),
        };
        state.test_rx = None;
        unsafe {
            let _ = EnableWindow(state.test_button, true);
        }
        state.notice = match outcome {
            Ok(steps) => Notice::Steps(steps),
            Err(()) => Notice::Line {
                ok: false,
                text: t(
                    "the connection-check thread died unexpectedly",
                    "接続チェックのスレッドが異常終了した",
                )
                .to_string(),
            },
        };
        unsafe {
            relayout(self.hwnd, state);
            scroll_to_bottom(state);
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
            let state = Box::from_raw(self.state);
            state.fonts.delete();
            for brush in [state.page_brush, state.card_brush, state.field_brush] {
                if !brush.is_invalid() {
                    let _ = DeleteObject(brush.into());
                }
            }
            if !state.icon.is_invalid() {
                let _ = DestroyIcon(state.icon);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Window classes and small helpers
// ---------------------------------------------------------------------------------------------

unsafe fn module_handle() -> Result<HINSTANCE> {
    Ok(
        unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None) }
            .context("cannot get the module handle")?
            .into(),
    )
}

fn frame_class() -> PCWSTR {
    w!("ScreenMemorySettings")
}

fn content_class() -> PCWSTR {
    w!("ScreenMemorySettingsContent")
}

fn frame_style() -> WINDOW_STYLE {
    WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX | WS_THICKFRAME
}

fn register_classes(instance: HINSTANCE) -> Result<()> {
    static REGISTERED: OnceLock<bool> = OnceLock::new();
    let ok = *REGISTERED.get_or_init(|| unsafe {
        // Both classes paint their own background, so no class brush: it would flash the wrong
        // colour on a theme switch and fight the double-buffered content paint.
        let frame = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(frame_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH::default(),
            lpszClassName: frame_class(),
            ..Default::default()
        };
        let content = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(content_proc),
            hInstance: instance,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH::default(),
            lpszClassName: content_class(),
            ..Default::default()
        };
        RegisterClassExW(&frame) != 0 && RegisterClassExW(&content) != 0
    });
    anyhow::ensure!(ok, "cannot register the settings window classes");
    Ok(())
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain([0]).collect()
}

/// DrawTextW takes a length, so the text it paints must not carry a terminator.
fn wide_raw(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn get_text(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let n = GetWindowTextW(hwnd, &mut buf).max(0) as usize;
        String::from_utf16_lossy(&buf[..n])
    }
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

/// `saved_message()` and the shared error strings lead with a status emoji, which the result
/// panel draws itself in the theme's colour. Strip it so it is not shown twice.
fn strip_mark(text: &str) -> String {
    text.trim_start_matches(['✅', '❌', '⚠'])
        .trim_start()
        .to_string()
}

fn state_of(hwnd: HWND) -> Option<&'static mut State> {
    let raw = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;
    unsafe { raw.as_mut() }
}

// ---------------------------------------------------------------------------------------------
// Children
// ---------------------------------------------------------------------------------------------

/// Create the content host and the real controls. `layout_content()` decides where they go.
unsafe fn build_children(hwnd: HWND, st: &mut State, values: &[String], instance: HINSTANCE) {
    // WS_EX_CONTROLPARENT is what lets IsDialogMessageW's tab traversal descend into the
    // scrolling area; without it Tab would only ever reach the two footer buttons.
    st.content = unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            content_class(),
            PCWSTR::null(),
            WS_CHILD | WS_VISIBLE | WS_VSCROLL,
            0,
            0,
            0,
            0,
            Some(hwnd),
            None,
            Some(instance),
            None,
        )
    }
    .unwrap_or_default();
    unsafe {
        SetWindowLongPtrW(st.content, GWLP_USERDATA, (st as *mut State) as isize);
    }

    let child = |parent: HWND, class: PCWSTR, text: &str, style: WINDOW_STYLE, id: usize| -> HWND {
        let text = wide(text);
        unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR(text.as_ptr()),
                WS_CHILD | WS_VISIBLE | style,
                0,
                0,
                0,
                0,
                Some(parent),
                (id != 0).then_some(HMENU(id as *mut _)),
                Some(instance),
                None,
            )
        }
        .unwrap_or_default()
    };

    for (field, value) in fields().iter().zip(values) {
        let edit = child(
            st.content,
            w!("EDIT"),
            value,
            // No WS_BORDER: the box around the input is painted by the content window
            WS_TABSTOP | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            0,
        );
        st.inputs.push((field.key, edit));
    }
    st.lang_combo = child(
        st.content,
        w!("COMBOBOX"),
        "",
        WS_TABSTOP | WINDOW_STYLE(CBS_DROPDOWNLIST as u32),
        0,
    );
    for title in language_options() {
        let text = wide(title);
        unsafe {
            SendMessageW(
                st.lang_combo,
                CB_ADDSTRING,
                None,
                Some(LPARAM(text.as_ptr() as isize)),
            );
        }
    }
    unsafe {
        SendMessageW(st.lang_combo, CB_SETCURSEL, Some(WPARAM(language_index())), None);
    }
    st.test_button = child(
        hwnd,
        w!("BUTTON"),
        t("Connection check", "接続チェック"),
        WS_TABSTOP,
        ID_TEST,
    );
    st.save_button = child(
        hwnd,
        w!("BUTTON"),
        t("Save", "保存"),
        WS_TABSTOP | WINDOW_STYLE(BS_DEFPUSHBUTTON as u32),
        ID_SAVE,
    );
    unsafe { apply_fonts(st) };
}

/// Push the fonts and the text inset into the controls. Both depend on the dpi, so this runs
/// again after WM_DPICHANGED.
unsafe fn apply_fonts(st: &State) {
    let set = |h: HWND, f: HFONT| unsafe {
        SendMessageW(h, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
    };
    for (_, edit) in &st.inputs {
        set(*edit, st.fonts.mono);
        // The inset comes from where layout_content places the EDIT inside its painted box, so
        // the control's own margins have to be out of the way.
        unsafe {
            SendMessageW(
                *edit,
                EM_SETMARGINS,
                Some(WPARAM((EC_LEFTMARGIN | EC_RIGHTMARGIN) as usize)),
                Some(LPARAM(0)),
            );
        }
    }
    for h in [st.lang_combo, st.test_button, st.save_button] {
        set(h, st.fonts.body);
    }
    // A CBS_DROPDOWNLIST combo decides its own closed height from the item height, and the themed
    // control on Windows 11 does not necessarily honour CB_SETITEMHEIGHT. Ask for the taller
    // selection field anyway, then read back what the control actually settled on: layout centres
    // it in the row using that, so the result is balanced whether or not the request took.
    unsafe {
        SendMessageW(
            st.lang_combo,
            CB_SETITEMHEIGHT,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM((st.scale(CTRL_H) - st.scale(COMBO_CHROME)) as isize)),
        );
    }
}

/// The height a combo box actually occupies when closed. `rcItem` is the selection field inside
/// the control's client area, so the field plus the equal inset above and below it is the whole
/// closed control.
unsafe fn combo_height(combo: HWND, fallback: i32) -> i32 {
    let mut info = COMBOBOXINFO {
        cbSize: size_of::<COMBOBOXINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetComboBoxInfo(combo, &mut info) }.is_err() {
        return fallback;
    }
    let h = info.rcItem.bottom + info.rcItem.top;
    if h > 0 { h } else { fallback }
}

/// Follow the OS app theme: the title bar through DWM, the common controls through their dark
/// theme classes. Only documented API is used, so a control the shell does not darken simply
/// stays light rather than being drawn by hand.
unsafe fn apply_theme(hwnd: HWND, st: &mut State) {
    let dark = windows::core::BOOL::from(st.theme.dark);
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const _ as *const _,
            size_of::<windows::core::BOOL>() as u32,
        )
    };
    // "CFD" is the theme class the common controls use for text fields and combo boxes;
    // "Explorer" additionally covers the content window's scroll bar.
    let field_theme = if st.theme.dark {
        w!("DarkMode_CFD")
    } else {
        w!("CFD")
    };
    let explorer_theme = if st.theme.dark {
        w!("DarkMode_Explorer")
    } else {
        w!("Explorer")
    };
    for (_, edit) in &st.inputs {
        let _ = unsafe { SetWindowTheme(*edit, field_theme, PCWSTR::null()) };
    }
    let _ = unsafe { SetWindowTheme(st.lang_combo, field_theme, PCWSTR::null()) };
    let _ = unsafe { SetWindowTheme(st.content, explorer_theme, PCWSTR::null()) };
    for h in [st.test_button, st.save_button] {
        let _ = unsafe { SetWindowTheme(h, explorer_theme, PCWSTR::null()) };
    }
}

/// Give the window the same badge the notification area shows, so the taskbar and Alt-Tab do
/// not fall back to the generic application icon.
unsafe fn set_window_icon(hwnd: HWND, st: &mut State) {
    const SIZE: u32 = 32;
    let rgba = art::render_badge(SIZE, art::ACTIVE_COLOR, art::camera_ink);
    let Some(icon) = (unsafe { icon_from_rgba(&rgba, SIZE) }) else {
        return;
    };
    st.icon = icon;
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_BIG as usize)),
            Some(LPARAM(icon.0 as isize)),
        );
        SendMessageW(
            hwnd,
            WM_SETICON,
            Some(WPARAM(ICON_SMALL as usize)),
            Some(LPARAM(icon.0 as isize)),
        );
    }
}

/// Build an HICON from straight-alpha RGBA. The colour bitmap wants premultiplied BGRA; the
/// mask goes unused for a 32-bit icon but CreateIconIndirect still insists on one.
unsafe fn icon_from_rgba(rgba: &[u8], size: u32) -> Option<HICON> {
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size as i32,
            biHeight: -(size as i32), // negative: top-down, matching render_badge's row order
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    let color = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0) }.ok()?;
    let count = (size * size) as usize;
    let dst = unsafe { std::slice::from_raw_parts_mut(bits as *mut u8, count * 4) };
    for i in 0..count {
        let (r, g, b, a) = (rgba[i * 4], rgba[i * 4 + 1], rgba[i * 4 + 2], rgba[i * 4 + 3]);
        let pre = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
        dst[i * 4] = pre(b);
        dst[i * 4 + 1] = pre(g);
        dst[i * 4 + 2] = pre(r);
        dst[i * 4 + 3] = a;
    }
    let mask = unsafe { CreateBitmap(size as i32, size as i32, 1, 1, None) };
    let info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: color,
    };
    let icon = unsafe { CreateIconIndirect(&info) }.ok();
    unsafe {
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
    }
    icon
}

// ---------------------------------------------------------------------------------------------
// Layout
// ---------------------------------------------------------------------------------------------

fn footer_h(st: &State) -> i32 {
    st.scale(FOOTER_PAD * 2 + BUTTON_H)
}

/// Position the footer buttons and the content host, then lay the page out inside it.
unsafe fn relayout(hwnd: HWND, st: &mut State) {
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let cw = client.right - client.left;
    let ch = client.bottom - client.top;
    let footer = footer_h(st);
    let view_h = (ch - footer).max(0);

    let _ = unsafe { MoveWindow(st.content, 0, 0, cw, view_h, true) };

    let bw = st.scale(BUTTON_W);
    let bh = st.scale(BUTTON_H);
    let by = ch - footer + st.scale(FOOTER_PAD);
    let save_x = cw - st.scale(PAGE_PAD) - bw;
    let _ = unsafe {
        MoveWindow(
            st.test_button,
            save_x - st.scale(BUTTON_GAP) - bw,
            by,
            bw,
            bh,
            true,
        )
    };
    let _ = unsafe { MoveWindow(st.save_button, save_x, by, bw, bh, true) };

    unsafe { layout_content(st, cw, view_h) };
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
        let _ = InvalidateRect(Some(st.content), None, false);
    }
}

/// Lays text into the display list, advancing by the height the text actually needs.
///
/// A line box is taller than the glyphs inside it, so a gap specified against a card edge or a
/// divider comes out looking wider next to text than next to a control. The text box is therefore
/// nudged up by part of the font's internal leading. Half of it is the useful amount: subtracting
/// all of it overshoots, because the CJK faces this window runs under put ink much closer to the
/// top of their line box than the metric implies. Measured on a Japanese Windows 11 at 125%, the
/// three gaps inside a card come out 20 / 20 / 19 pixels against a nominal 20.
#[derive(Clone, Copy)]
struct TextPen {
    hdc: HDC,
    fonts: Fonts,
    /// tmHeight per face, indexed by `Face::index`, plus the input font in the last slot.
    line: [i32; 4],
    /// How far to lift a text box so its ink lines up with the nominal position.
    lift: [i32; 3],
}

/// Index of the input box font in `TextPen::line`.
const MONO_LINE: usize = 3;

impl TextPen {
    unsafe fn new(hdc: HDC, fonts: Fonts) -> TextPen {
        let mut line = [0; 4];
        let mut lift = [0; 3];
        for face in [Face::Heading, Face::Label, Face::Caption] {
            let tm = unsafe { font_metrics(hdc, fonts.get(face)) };
            line[face.index()] = tm.0;
            lift[face.index()] = tm.1 / 2;
        }
        line[MONO_LINE] = unsafe { font_metrics(hdc, fonts.mono) }.0;
        TextPen {
            hdc,
            fonts,
            line,
            lift,
        }
    }

    /// Place `text` at `y` and return the y directly below it.
    unsafe fn put(
        &self,
        draws: &mut Vec<Draw>,
        x: i32,
        y: i32,
        w: i32,
        text: &str,
        face: Face,
        tone: Tone,
        flags: DRAW_TEXT_FORMAT,
    ) -> i32 {
        let boxed =
            unsafe { text_h(self.hdc, self.fonts.get(face), text, w) }.max(self.line[face.index()]);
        let lift = self.lift[face.index()];
        draws.push(Draw::Text {
            rect: rect(x, y - lift, w, boxed),
            text: text.to_string(),
            face,
            tone,
            flags,
        });
        y + boxed - lift
    }
}

/// (tmHeight, tmInternalLeading) for `font`.
unsafe fn font_metrics(hdc: HDC, font: HFONT) -> (i32, i32) {
    let old = unsafe { SelectObject(hdc, font.into()) };
    let mut tm = TEXTMETRICW::default();
    let _ = unsafe { GetTextMetricsW(hdc, &mut tm) };
    unsafe { SelectObject(hdc, old) };
    (tm.tmHeight, tm.tmInternalLeading)
}

/// Build the display list and the control placements for a content viewport `width` x `view_h`.
unsafe fn layout_content(st: &mut State, width: i32, view_h: i32) {
    let hdc = unsafe { GetDC(Some(st.content)) };
    let pen = unsafe { TextPen::new(hdc, st.fonts) };
    let mut draws: Vec<Draw> = Vec::new();
    let mut places: Vec<Placement> = Vec::new();

    let pad = st.scale(PAGE_PAD);
    // Leave room for the scroll bar so text never slides under it
    let bar = unsafe { windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics(SM_CXVSCROLL) };
    let inner_w = (width - pad * 2 - bar).max(st.scale(200));
    let card_pad = st.scale(CARD_PAD);
    let card_inner = inner_w - card_pad * 2;
    let mut y = pad;

    let single = DT_SINGLELINE | DT_NOPREFIX;
    let wrapped = DT_WORDBREAK | DT_NOPREFIX;

    // Page description and where the file lives
    y = unsafe {
        pen.put(
            &mut draws,
            pad,
            y,
            inner_w,
            intro(),
            Face::Label,
            Tone::Text,
            wrapped,
        )
    };
    y += st.scale(6);
    let path = format!(
        "{}: {}",
        t("File", "保存先"),
        default_config_path().display()
    );
    y = unsafe {
        pen.put(
            &mut draws,
            pad,
            y,
            inner_w,
            &path,
            Face::Caption,
            Tone::Sub,
            wrapped,
        )
    };
    y += st.scale(SECTION_GAP);

    if st.broken {
        y = unsafe {
            push_notice_card(
                st,
                &pen,
                &mut draws,
                pad,
                y,
                inner_w,
                None,
                &[(false, None, broken_note().to_string())],
            )
        };
    }

    let all = fields();
    for (title, keys) in sections() {
        y = unsafe { push_heading(st, &pen, &mut draws, pad, y, inner_w, title) };
        let card_at = draws.len();
        draws.push(Draw::Card(RECT::default()));
        let card_top = y;
        // Both card_pad gaps are now ink-to-edge, so the top and bottom insets look equal
        let mut cy = y + card_pad;
        for (i, key) in keys.iter().enumerate() {
            let Some(field) = all.iter().find(|f| f.key == *key) else {
                continue;
            };
            let Some((_, edit)) = st.inputs.iter().find(|(k, _)| k == key) else {
                continue;
            };
            cy = unsafe {
                pen.put(
                    &mut draws,
                    pad + card_pad,
                    cy,
                    card_inner,
                    field.label,
                    Face::Label,
                    Tone::Text,
                    single | DT_END_ELLIPSIS,
                )
            };
            if !field.hint.is_empty() {
                cy += st.scale(3);
                cy = unsafe {
                    pen.put(
                        &mut draws,
                        pad + card_pad,
                        cy,
                        card_inner,
                        field.hint,
                        Face::Caption,
                        Tone::Sub,
                        wrapped,
                    )
                };
            }
            cy += st.scale(8);
            // The painted box is the full 32dip; the EDIT itself is one line tall and sits
            // centred inside it, which is the only way to get even space above and below the text
            let field = rect(pad + card_pad, cy, card_inner, st.scale(CTRL_H));
            draws.push(Draw::Field {
                rect: field,
                hwnd: *edit,
            });
            let inset = st.scale(EDIT_INSET);
            let line = pen.line[MONO_LINE];
            places.push(Placement {
                hwnd: *edit,
                rect: rect(
                    field.left + inset,
                    field.top + (st.scale(CTRL_H) - line) / 2,
                    card_inner - inset * 2,
                    line,
                ),
                drop_h: 0,
            });
            cy += st.scale(CTRL_H);
            if i + 1 < keys.len() {
                cy += card_pad;
                draws.push(Draw::Divider(rect(pad, cy, inner_w, st.scale(1).max(1))));
                cy += card_pad;
            }
        }
        cy += card_pad;
        draws[card_at] = Draw::Card(rect(pad, card_top, inner_w, cy - card_top));
        y = cy + st.scale(SECTION_GAP);
    }

    // Language: a Win11-style row, label on the left and the control on the right
    y = unsafe {
        push_heading(
            st,
            &pen,
            &mut draws,
            pad,
            y,
            inner_w,
            t("Appearance", "表示"),
        )
    };
    let row_h = card_pad * 2 + st.scale(CTRL_H);
    draws.push(Draw::Card(rect(pad, y, inner_w, row_h)));
    let combo_w = st.scale(COMBO_W).min(card_inner - st.scale(80));
    // DT_VCENTER does its own leading compensation, so this one is placed by its box
    draws.push(Draw::Text {
        rect: rect(
            pad + card_pad,
            y + card_pad,
            card_inner - combo_w - st.scale(12),
            st.scale(CTRL_H),
        ),
        text: language_label().to_string(),
        face: Face::Label,
        tone: Tone::Text,
        flags: single | DT_VCENTER | DT_END_ELLIPSIS,
    });
    // Centre the combo in the 32dip row, since it is free to be shorter than that
    let combo_h = unsafe { combo_height(st.lang_combo, st.scale(CTRL_H)) };
    places.push(Placement {
        hwnd: st.lang_combo,
        rect: rect(
            pad + inner_w - card_pad - combo_w,
            y + card_pad + (st.scale(CTRL_H) - combo_h) / 2,
            combo_w,
            combo_h,
        ),
        drop_h: st.scale(COMBO_DROP_H),
    });
    y += row_h + st.scale(SECTION_GAP);

    // The result panel, only once there is something to say
    let lines: Vec<(bool, Option<String>, String)> = match &st.notice {
        Notice::None => Vec::new(),
        Notice::Line { ok, text } => vec![(*ok, None, strip_mark(text))],
        Notice::Steps(steps) => steps
            .iter()
            .map(|s| (s.ok, Some(s.name.to_string()), s.message.clone()))
            .collect(),
    };
    if !lines.is_empty() {
        y = unsafe {
            push_notice_card(
                st,
                &pen,
                &mut draws,
                pad,
                y,
                inner_w,
                Some(t("Result", "結果")),
                &lines,
            )
        };
    }

    unsafe { ReleaseDC(Some(st.content), hdc) };
    st.draws = draws;
    st.places = places;
    st.content_h = y - st.scale(SECTION_GAP) + pad;
    unsafe { apply_scroll(st, view_h) };
}

unsafe fn push_heading(
    st: &State,
    pen: &TextPen,
    draws: &mut Vec<Draw>,
    x: i32,
    y: i32,
    w: i32,
    text: &str,
) -> i32 {
    let y = unsafe {
        pen.put(
            draws,
            x,
            y,
            w,
            text,
            Face::Heading,
            Tone::Text,
            DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        )
    };
    y + st.scale(10)
}

/// A card of status lines: a ✓ / ✕ in the theme colour, an optional step name, then the message.
/// Returns the y below the card.
#[allow(clippy::too_many_arguments)]
unsafe fn push_notice_card(
    st: &State,
    pen: &TextPen,
    draws: &mut Vec<Draw>,
    x: i32,
    mut y: i32,
    w: i32,
    heading: Option<&str>,
    lines: &[(bool, Option<String>, String)],
) -> i32 {
    if let Some(heading) = heading {
        y = unsafe { push_heading(st, pen, draws, x, y, w, heading) };
    }
    let card_pad = st.scale(CARD_PAD);
    let card_at = draws.len();
    draws.push(Draw::Card(RECT::default()));
    let card_top = y;
    let mut cy = y + card_pad;
    let mark_w = st.scale(22);
    let text_x = x + card_pad + mark_w;
    let text_w = w - card_pad * 2 - mark_w;
    for (i, (ok, name, message)) in lines.iter().enumerate() {
        // The mark shares the head's baseline, so place it at the same ink top
        unsafe {
            pen.put(
                draws,
                x + card_pad,
                cy,
                mark_w,
                if *ok { "✓" } else { "✕" },
                Face::Label,
                if *ok { Tone::Ok } else { Tone::Ng },
                DT_SINGLELINE | DT_NOPREFIX,
            )
        };
        let head = name.clone().unwrap_or_else(|| message.clone());
        cy = unsafe {
            pen.put(
                draws,
                text_x,
                cy,
                text_w,
                &head,
                Face::Label,
                Tone::Text,
                DT_WORDBREAK | DT_NOPREFIX,
            )
        };
        if name.is_some() {
            cy += st.scale(3);
            cy = unsafe {
                pen.put(
                    draws,
                    text_x,
                    cy,
                    text_w,
                    message,
                    Face::Caption,
                    Tone::Sub,
                    DT_WORDBREAK | DT_NOPREFIX,
                )
            };
        }
        if i + 1 < lines.len() {
            cy += st.scale(14);
        }
    }
    cy += card_pad;
    draws[card_at] = Draw::Card(rect(x, card_top, w, cy - card_top));
    cy + st.scale(SECTION_GAP)
}

/// Height `text` needs when wrapped to `width`, in the given font.
unsafe fn text_h(hdc: HDC, font: HFONT, text: &str, width: i32) -> i32 {
    if text.is_empty() {
        return 0;
    }
    let old = unsafe { SelectObject(hdc, font.into()) };
    let mut buf = wide_raw(text);
    let mut r = rect(0, 0, width, 0);
    unsafe {
        DrawTextW(
            hdc,
            &mut buf,
            &mut r,
            windows::Win32::Graphics::Gdi::DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        SelectObject(hdc, old);
    }
    r.bottom - r.top
}

/// Clamp the scroll offset, move the controls to match, and refresh the scroll bar.
unsafe fn apply_scroll(st: &mut State, view_h: i32) {
    let max_scroll = (st.content_h - view_h).max(0);
    st.scroll_y = st.scroll_y.clamp(0, max_scroll);
    for p in &st.places {
        let _ = unsafe {
            MoveWindow(
                p.hwnd,
                p.rect.left,
                p.rect.top - st.scroll_y,
                p.rect.right - p.rect.left,
                (p.rect.bottom - p.rect.top) + p.drop_h,
                true,
            )
        };
    }
    let info = SCROLLINFO {
        cbSize: size_of::<SCROLLINFO>() as u32,
        fMask: SIF_RANGE | SIF_PAGE | SIF_POS,
        nMin: 0,
        nMax: (st.content_h - 1).max(0),
        nPage: view_h.max(0) as u32,
        nPos: st.scroll_y,
        nTrackPos: 0,
    };
    unsafe { SetScrollInfo(st.content, SB_VERT, &info, true) };
}

fn viewport_h(st: &State) -> i32 {
    let mut r = RECT::default();
    let _ = unsafe { GetClientRect(st.content, &mut r) };
    r.bottom - r.top
}

unsafe fn scroll_by(st: &mut State, delta: i32) {
    let view = viewport_h(st);
    let before = st.scroll_y;
    st.scroll_y += delta;
    unsafe { apply_scroll(st, view) };
    if st.scroll_y != before {
        let _ = unsafe { InvalidateRect(Some(st.content), None, false) };
    }
}

unsafe fn scroll_to_bottom(st: &mut State) {
    let view = viewport_h(st);
    st.scroll_y = i32::MAX / 2;
    unsafe { apply_scroll(st, view) };
    let _ = unsafe { InvalidateRect(Some(st.content), None, false) };
}

/// Keep a control that just took focus inside the viewport, so tabbing never lands on something
/// the user cannot see.
unsafe fn ensure_visible(st: &mut State, target: HWND) {
    let Some(p) = st.places.iter().find(|p| p.hwnd == target) else {
        return;
    };
    let (top, bottom) = (p.rect.top, p.rect.bottom);
    let margin = st.scale(CARD_PAD);
    let view = viewport_h(st);
    let want = if top - st.scroll_y < margin {
        top - margin
    } else if bottom - st.scroll_y > view - margin {
        bottom - view + margin
    } else {
        return;
    };
    st.scroll_y = want;
    unsafe { apply_scroll(st, view) };
    let _ = unsafe { InvalidateRect(Some(st.content), None, false) };
}

/// Size the frame to the laid-out page (capped, so a long form scrolls instead of running off
/// the screen) and centre it on the monitor it landed on.
unsafe fn size_to_content(hwnd: HWND, st: &mut State) {
    let client_w = st.scale(PAGE_W);
    let client_h = (st.content_h + footer_h(st)).min(st.scale(MAX_H));
    let mut r = rect(0, 0, client_w, client_h);
    let _ = unsafe {
        AdjustWindowRectExForDpi(
            &mut r,
            frame_style(),
            false,
            WINDOW_EX_STYLE::default(),
            st.dpi,
        )
    };
    let (w, h) = (r.right - r.left, r.bottom - r.top);

    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let (x, y) = if unsafe { GetMonitorInfoW(monitor, &mut mi) }.as_bool() {
        let work = mi.rcWork;
        (
            work.left + ((work.right - work.left) - w) / 2,
            (work.top + ((work.bottom - work.top) - h) / 2).max(work.top),
        )
    } else {
        (CW_USEDEFAULT, CW_USEDEFAULT)
    };
    let _ = unsafe { SetWindowPos(hwnd, None, x, y, w, h, SWP_NOZORDER | SWP_NOACTIVATE) };
    unsafe { relayout(hwnd, st) };
}

// ---------------------------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------------------------

unsafe fn paint_frame(hwnd: HWND, st: &State) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let footer = footer_h(st);
    let top = client.bottom - footer;
    // The content window covers everything above the footer, so only the bar needs painting
    let bar = rect(0, top, client.right - client.left, footer);
    unsafe {
        FillRect(hdc, &bar, st.card_brush);
        let line = rect(0, top, client.right - client.left, st.scale(1).max(1));
        let border = CreateSolidBrush(st.theme.border);
        FillRect(hdc, &line, border);
        let _ = DeleteObject(border.into());
        let _ = EndPaint(hwnd, &ps);
    }
}

/// Paint the page into an off-screen bitmap first: the cards, dividers and text overlap the
/// background, and the controls sit on top, so painting straight to the screen would flicker.
unsafe fn paint_content(hwnd: HWND, st: &State) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    let mut client = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut client) };
    let (w, h) = (client.right, client.bottom);
    if w <= 0 || h <= 0 {
        let _ = unsafe { EndPaint(hwnd, &ps) };
        return;
    }
    let mem = unsafe { CreateCompatibleDC(Some(hdc)) };
    let bmp = unsafe { CreateCompatibleBitmap(hdc, w, h) };
    let old_bmp = unsafe { SelectObject(mem, bmp.into()) };
    unsafe {
        FillRect(mem, &client, st.page_brush);
        SetBkMode(mem, TRANSPARENT);
    }

    let radius = st.scale(CARD_RADIUS) * 2;
    for item in &st.draws {
        match item {
            Draw::Card(r) => {
                let r = shift(*r, st.scroll_y);
                if r.bottom < 0 || r.top > h {
                    continue;
                }
                unsafe {
                    let pen = CreatePen(PS_SOLID, st.scale(1).max(1), st.theme.border);
                    let old_pen = SelectObject(mem, pen.into());
                    let old_brush = SelectObject(mem, st.card_brush.into());
                    let _ = RoundRect(mem, r.left, r.top, r.right, r.bottom, radius, radius);
                    SelectObject(mem, old_brush);
                    SelectObject(mem, old_pen);
                    let _ = DeleteObject(pen.into());
                }
            }
            Draw::Divider(r) => {
                let r = shift(*r, st.scroll_y);
                if r.bottom < 0 || r.top > h {
                    continue;
                }
                unsafe {
                    let brush = CreateSolidBrush(st.theme.border);
                    FillRect(mem, &r, brush);
                    let _ = DeleteObject(brush.into());
                }
            }
            Draw::Field { rect: r, hwnd } => {
                let r = shift(*r, st.scroll_y);
                if r.bottom < 0 || r.top > h {
                    continue;
                }
                let focused = st.focus == *hwnd;
                let field_radius = st.scale(4) * 2;
                unsafe {
                    // Windows 11 keeps a focused text box's outline in the ordinary border colour
                    // and marks focus with an accent bar along the bottom edge only
                    let pen = CreatePen(PS_SOLID, st.scale(1).max(1), st.theme.border);
                    let old_pen = SelectObject(mem, pen.into());
                    let old_brush = SelectObject(mem, st.field_brush.into());
                    let _ = RoundRect(
                        mem,
                        r.left,
                        r.top,
                        r.right,
                        r.bottom,
                        field_radius,
                        field_radius,
                    );
                    SelectObject(mem, old_brush);
                    SelectObject(mem, old_pen);
                    let _ = DeleteObject(pen.into());
                    if focused {
                        let thick = st.scale(2).max(2);
                        let inset = st.scale(3);
                        let brush = CreateSolidBrush(COLORREF(GetSysColor(COLOR_HIGHLIGHT)));
                        let line = RECT {
                            left: r.left + inset,
                            top: r.bottom - thick,
                            right: r.right - inset,
                            bottom: r.bottom,
                        };
                        FillRect(mem, &line, brush);
                        let _ = DeleteObject(brush.into());
                    }
                }
            }
            Draw::Text {
                rect: r,
                text,
                face,
                tone,
                flags,
            } => {
                let mut r = shift(*r, st.scroll_y);
                if r.bottom < 0 || r.top > h {
                    continue;
                }
                unsafe {
                    let old = SelectObject(mem, st.fonts.get(*face).into());
                    SetTextColor(mem, st.theme.tone(*tone));
                    let mut buf = wide_raw(text);
                    DrawTextW(mem, &mut buf, &mut r, *flags);
                    SelectObject(mem, old);
                }
            }
        }
    }

    unsafe {
        let _ = BitBlt(hdc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old_bmp);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        let _ = EndPaint(hwnd, &ps);
    }
}

fn shift(r: RECT, scroll_y: i32) -> RECT {
    RECT {
        left: r.left,
        top: r.top - scroll_y,
        right: r.right,
        bottom: r.bottom - scroll_y,
    }
}

/// Shared by the WM_CTLCOLOR* handlers: paint the control's text in the theme colours and hand
/// back the brush the control should fill itself with.
unsafe fn ctl_color(hdc: HDC, text: COLORREF, back: COLORREF, brush: HBRUSH) -> LRESULT {
    unsafe {
        SetTextColor(hdc, text);
        SetBkColor(hdc, back);
    }
    LRESULT(brush.0 as isize)
}

// ---------------------------------------------------------------------------------------------
// Window procedures
// ---------------------------------------------------------------------------------------------

unsafe extern "system" fn frame_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_NCCREATE => {
                // Hook CreateWindowExW's lpparam (the State pointer) up to USERDATA
                let create = lparam.0 as *const CREATESTRUCTW;
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*create).lpCreateParams as isize);
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                if let Some(st) = state_of(hwnd) {
                    paint_frame(hwnd, st);
                }
                LRESULT(0)
            }
            WM_SIZE => {
                if let Some(st) = state_of(hwnd) {
                    relayout(hwnd, st);
                }
                LRESULT(0)
            }
            WM_GETMINMAXINFO => {
                let dpi = GetDpiForWindow(hwnd);
                let dpi = if dpi == 0 { 96 } else { dpi };
                let mut r = rect(0, 0, MIN_W * dpi as i32 / 96, MIN_H * dpi as i32 / 96);
                let _ = AdjustWindowRectExForDpi(
                    &mut r,
                    frame_style(),
                    false,
                    WINDOW_EX_STYLE::default(),
                    dpi,
                );
                let info = &mut *(lparam.0 as *mut MINMAXINFO);
                info.ptMinTrackSize.x = r.right - r.left;
                info.ptMinTrackSize.y = r.bottom - r.top;
                LRESULT(0)
            }
            // The footer buttons are the frame's own children
            WM_CTLCOLORBTN => {
                let Some(st) = state_of(hwnd) else {
                    return LRESULT(0);
                };
                ctl_color(
                    HDC(wparam.0 as *mut _),
                    st.theme.text,
                    st.theme.card,
                    st.card_brush,
                )
            }
            WM_COMMAND => {
                let Some(st) = state_of(hwnd) else {
                    return LRESULT(0);
                };
                match wparam.0 & 0xffff {
                    ID_TEST => {
                        let _ = EnableWindow(st.test_button, false);
                        st.notice = Notice::Line {
                            ok: true,
                            text: testing_message().to_string(),
                        };
                        st.test_rx = Some(spawn_test(st.values()));
                        relayout(hwnd, st);
                        scroll_to_bottom(st);
                    }
                    ID_SAVE | ID_ENTER => {
                        st.notice = match save(&st.values()) {
                            Ok(()) => {
                                let index = SendMessageW(st.lang_combo, CB_GETCURSEL, None, None)
                                    .0
                                    .max(0) as usize;
                                let lang_changed = save_language(index);
                                let _ = st.saved_tx.send(());
                                Notice::Line {
                                    ok: true,
                                    text: saved_message(lang_changed),
                                }
                            }
                            Err(e) => Notice::Line {
                                ok: false,
                                text: format!("{e:#}"),
                            },
                        };
                        relayout(hwnd, st);
                        scroll_to_bottom(st);
                    }
                    ID_ESC => {
                        let _ = DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED => {
                // ImmersiveColorSet is how the shell announces a light / dark switch
                let relevant = msg == WM_THEMECHANGED
                    || (lparam.0 != 0
                        && PCWSTR(lparam.0 as *const u16)
                            .to_string()
                            .map(|s| s == "ImmersiveColorSet")
                            .unwrap_or(false));
                if relevant && let Some(st) = state_of(hwnd) {
                    st.theme = Theme::load();
                    st.rebuild_brushes();
                    apply_theme(hwnd, st);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    let _ = InvalidateRect(Some(st.content), None, false);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DPICHANGED => {
                // Take the OS's suggested position; relayout redecides the rest
                let suggested = &*(lparam.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                if let Some(st) = state_of(hwnd) {
                    st.dpi = (wparam.0 & 0xffff) as u32;
                    let old = st.fonts;
                    st.fonts = Fonts::build(st.dpi);
                    old.delete();
                    apply_fonts(st);
                    relayout(hwnd, st);
                }
                LRESULT(0)
            }
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                DIALOG.store(0, Ordering::Relaxed);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

unsafe extern "system" fn content_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    unsafe {
        match msg {
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                if let Some(st) = state_of(hwnd) {
                    paint_content(hwnd, st);
                } else {
                    let mut ps = PAINTSTRUCT::default();
                    BeginPaint(hwnd, &mut ps);
                    let _ = EndPaint(hwnd, &ps);
                }
                LRESULT(0)
            }
            WM_CTLCOLOREDIT => {
                let Some(st) = state_of(hwnd) else {
                    return LRESULT(0);
                };
                ctl_color(
                    HDC(wparam.0 as *mut _),
                    st.theme.text,
                    st.theme.field,
                    st.field_brush,
                )
            }
            // The combo box's drop-down list, and the static part of a CBS_DROPDOWNLIST combo
            WM_CTLCOLORLISTBOX | WM_CTLCOLORSTATIC => {
                let Some(st) = state_of(hwnd) else {
                    return LRESULT(0);
                };
                ctl_color(
                    HDC(wparam.0 as *mut _),
                    st.theme.text,
                    st.theme.field,
                    st.field_brush,
                )
            }
            WM_VSCROLL => {
                let Some(st) = state_of(hwnd) else {
                    return LRESULT(0);
                };
                let code = (wparam.0 & 0xffff) as u32;
                let view = viewport_h(st);
                let step = st.scale(SCROLL_STEP);
                match code {
                    SB_LINEUP => scroll_by(st, -step),
                    SB_LINEDOWN => scroll_by(st, step),
                    SB_PAGEUP => scroll_by(st, -view),
                    SB_PAGEDOWN => scroll_by(st, view),
                    SB_THUMBPOSITION | SB_THUMBTRACK => {
                        // nPos is 16-bit in wparam, so read the full value back instead
                        let mut info = SCROLLINFO {
                            cbSize: size_of::<SCROLLINFO>() as u32,
                            fMask: SIF_ALL,
                            ..Default::default()
                        };
                        if GetScrollInfo(hwnd, SB_VERT, &mut info).is_ok() {
                            scroll_by(st, info.nTrackPos - st.scroll_y);
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                if let Some(st) = state_of(hwnd) {
                    let delta = ((wparam.0 >> 16) & 0xffff) as u16 as i16;
                    let step = st.scale(SCROLL_STEP);
                    scroll_by(st, -(delta as i32) * step / 120);
                }
                LRESULT(0)
            }
            WM_COMMAND => {
                let notify = ((wparam.0 >> 16) & 0xffff) as u32;
                let control = HWND(lparam.0 as *mut _);
                if let Some(st) = state_of(hwnd) {
                    match notify {
                        EN_SETFOCUS => {
                            st.focus = control;
                            let _ = InvalidateRect(Some(hwnd), None, false);
                            ensure_visible(st, control);
                        }
                        EN_KILLFOCUS => {
                            if st.focus == control {
                                st.focus = HWND::default();
                            }
                            let _ = InvalidateRect(Some(hwnd), None, false);
                        }
                        n if n == CBN_SETFOCUS as u32 => ensure_visible(st, control),
                        _ => {}
                    }
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_mark_removes_the_status_emoji() {
        assert_eq!(strip_mark("✅ Saved"), "Saved");
        assert_eq!(strip_mark("❌ broken"), "broken");
        assert_eq!(strip_mark("plain"), "plain");
    }

    #[test]
    fn sections_cover_every_field() {
        let keys: Vec<&str> = sections()
            .iter()
            .flat_map(|(_, keys)| keys.iter().copied())
            .collect();
        assert_eq!(keys.len(), super::super::FIELD_COUNT);
        for field in fields() {
            assert!(keys.contains(&field.key), "{} is not in a section", field.key);
        }
    }
}
