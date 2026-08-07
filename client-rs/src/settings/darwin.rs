//! The native macOS settings window (AppKit).
//!
//! The tray (tray/darwin.rs) already runs the NSApplication event pump on the main thread, so
//! all we need here is one NSWindow on top of it. To receive the buttons' target-action we
//! define a single NSObject subclass (Controller) and hang the references to the input fields
//! and the connection check's receiver off it.

use std::cell::RefCell;
use std::sync::mpsc;

use anyhow::{Context, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezelStyle, NSBox, NSBoxType, NSButton, NSColor, NSFont,
    NSFontWeightRegular, NSLineBreakMode, NSMenu, NSMenuItem, NSPopUpButton, NSTextField,
    NSTextFieldBezelStyle, NSTitlePosition, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

use super::{
    Step, Values, broken_note, current_values, fields, format_steps, intro, language_index,
    language_label, language_options, save, save_language, saved_message, spawn_test,
    testing_message, window_title,
};
use crate::config::default_config_path;
use crate::i18n::t;

const WIDTH: f64 = 600.0;
const MARGIN: f64 = 20.0;
const LABEL_H: f64 = 16.0;
/// Height of one "label | input" row inside the group box.
const ROW_H: f64 = 40.0;
const FIELD_W: f64 = 340.0;
const BUTTON_W: f64 = 120.0;
const BUTTON_H: f64 = 30.0;
const RESULT_H: f64 = 100.0;
/// Height of the language selector row.
const LANG_H: f64 = 26.0;
const LANG_W: f64 = 200.0;

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

struct Ivars {
    /// The input fields, in `fields()` order.
    inputs: Vec<(&'static str, Retained<NSTextField>)>,
    language: Retained<NSPopUpButton>,
    result: Retained<NSTextField>,
    test_button: Retained<NSButton>,
    saved_tx: mpsc::Sender<()>,
    /// Some only while a connection check is running.
    test_rx: RefCell<Option<mpsc::Receiver<Vec<Step>>>>,
}

define_class!(
    /// An Objective-C class whose only job is to receive the buttons' target-action.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SMSettingsController"]
    #[ivars = Ivars]
    struct Controller;

    impl Controller {
        #[unsafe(method(onTest:))]
        fn on_test(&self, _sender: Option<&AnyObject>) {
            self.ivars().test_button.setEnabled(false);
            self.set_result(testing_message());
            *self.ivars().test_rx.borrow_mut() = Some(spawn_test(self.values()));
        }

        #[unsafe(method(onSave:))]
        fn on_save(&self, _sender: Option<&AnyObject>) {
            match save(&self.values()) {
                Ok(()) => {
                    let index = self.ivars().language.indexOfSelectedItem().max(0) as usize;
                    let lang_changed = save_language(index);
                    self.set_result(&saved_message(lang_changed));
                    let _ = self.ivars().saved_tx.send(());
                }
                Err(e) => self.set_result(&format!("❌ {e:#}")),
            }
        }
    }
);

impl Controller {
    fn new(
        mtm: MainThreadMarker,
        inputs: Vec<(&'static str, Retained<NSTextField>)>,
        language: Retained<NSPopUpButton>,
        result: Retained<NSTextField>,
        test_button: Retained<NSButton>,
        saved_tx: mpsc::Sender<()>,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            inputs,
            language,
            result,
            test_button,
            saved_tx,
            test_rx: RefCell::new(None),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn values(&self) -> Values {
        self.ivars()
            .inputs
            .iter()
            .map(|(key, input)| (*key, input.stringValue().to_string()))
            .collect()
    }

    fn set_result(&self, text: &str) {
        self.ivars().result.setStringValue(&ns(text));
    }
}

pub struct Window {
    mtm: MainThreadMarker,
    window: Retained<NSWindow>,
    controller: Retained<Controller>,
}

impl Window {
    /// Build the window and bring it to the front.
    pub fn open(saved_tx: mpsc::Sender<()>) -> Result<Window> {
        let mtm =
            MainThreadMarker::new().context("the settings window must open on the main thread")?;
        sync_appearance(mtm);
        ensure_edit_menu(mtm);
        let (values, broken) = current_values();

        // Lay out top to bottom. AppKit's origin is the bottom-left corner, so compute the
        // total height first and translate the cursor y_top (distance from the top edge) into
        // bottom-left-origin rectangles.
        let note_h = if broken { LABEL_H + 4.0 } else { 0.0 };
        let box_h = ROW_H * fields().len() as f64 + 4.0; // +4 for the NSBox border
        let height = MARGIN
            + LABEL_H          // intro text
            + 4.0 + LABEL_H    // config file path
            + note_h
            + 12.0
            + box_h
            + 14.0
            + LANG_H
            + 12.0
            + BUTTON_H
            + 12.0
            + RESULT_H
            + MARGIN;
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, height)),
                NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        // Don't let AppKit release the window on close; that would double-free against the
        // Retained on the Rust side. A closed window reports isVisible false instead, which is
        // how the tray loop knows to drop the Window
        unsafe { window.setReleasedWhenClosed(false) };
        window.setTitle(&ns(window_title()));
        let content = window.contentView().context("window has no contentView")?;
        let inner = WIDTH - MARGIN * 2.0;
        let rect = |y_top: f64, h: f64| {
            NSRect::new(
                NSPoint::new(MARGIN, height - y_top - h),
                NSSize::new(inner, h),
            )
        };

        let small = NSFont::systemFontOfSize(11.0);
        let body = NSFont::systemFontOfSize(13.0);
        let mono = NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular });

        let mut y = MARGIN;
        let intro_label = NSTextField::labelWithString(&ns(intro()), mtm);
        intro_label.setFrame(rect(y, LABEL_H));
        intro_label.setFont(Some(&body));
        content.addSubview(&intro_label);
        y += LABEL_H + 4.0;

        let path = NSTextField::labelWithString(
            &ns(&format!(
                "{}: {}",
                t("File", "保存先"),
                default_config_path().display()
            )),
            mtm,
        );
        path.setFrame(rect(y, LABEL_H));
        path.setFont(Some(&small));
        path.setTextColor(Some(&NSColor::secondaryLabelColor()));
        path.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        content.addSubview(&path);
        y += LABEL_H;

        if broken {
            y += 4.0;
            let note = NSTextField::labelWithString(&ns(broken_note()), mtm);
            note.setFrame(rect(y, LABEL_H));
            note.setFont(Some(&small));
            note.setTextColor(Some(&NSColor::systemRedColor()));
            note.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
            content.addSubview(&note);
            y += LABEL_H;
        }
        y += 12.0;

        // A System Settings-style group box holding the "label | input" rows, with hairline
        // separators between them
        let group = NSBox::initWithFrame(NSBox::alloc(mtm), rect(y, box_h));
        group.setTitlePosition(NSTitlePosition::NoTitle);
        group.setContentViewMargins(NSSize::new(0.0, 0.0));
        content.addSubview(&group);
        y += box_h + 14.0;

        let rows = group.contentView().context("NSBox has no contentView")?;
        let bounds = rows.bounds();
        let all_fields = fields();
        let mut inputs = Vec::with_capacity(all_fields.len());
        for (i, (field, value)) in all_fields.iter().zip(&values).enumerate() {
            // Translate the row's distance from the top into contentView's bottom-left origin
            let row_bottom = bounds.size.height - ROW_H * (i + 1) as f64;
            let label = NSTextField::labelWithString(&ns(field.label), mtm);
            label.setFrame(NSRect::new(
                NSPoint::new(16.0, row_bottom + (ROW_H - LABEL_H) / 2.0),
                NSSize::new(bounds.size.width - FIELD_W - 44.0, LABEL_H),
            ));
            label.setFont(Some(&body));
            rows.addSubview(&label);

            let input = NSTextField::initWithFrame(
                NSTextField::alloc(mtm),
                NSRect::new(
                    NSPoint::new(
                        bounds.size.width - 12.0 - FIELD_W,
                        row_bottom + (ROW_H - 22.0) / 2.0,
                    ),
                    NSSize::new(FIELD_W, 22.0),
                ),
            );
            input.setStringValue(&ns(value));
            input.setPlaceholderString(Some(&ns(field.hint)));
            input.setFont(Some(&mono));
            // Long values stay on one line and scroll horizontally rather than wrapping (the
            // rounded bezel is single-line only). Wrapping while editing is governed by the
            // cell, so turn wraps off there and make it scrollable
            input.setBezelStyle(NSTextFieldBezelStyle::RoundedBezel);
            input.setUsesSingleLineMode(true);
            if let Some(cell) = input.cell() {
                cell.setWraps(false);
                cell.setScrollable(true);
            }
            rows.addSubview(&input);
            inputs.push((field.key, input));

            if i + 1 < all_fields.len() {
                let separator = NSBox::initWithFrame(
                    NSBox::alloc(mtm),
                    NSRect::new(
                        NSPoint::new(16.0, row_bottom),
                        NSSize::new(bounds.size.width - 32.0, 1.0),
                    ),
                );
                separator.setBoxType(NSBoxType::Separator);
                rows.addSubview(&separator);
            }
        }

        // The language selector row, laid out like the rows above but outside the group box
        let lang_bottom = height - y - LANG_H;
        let lang_text = NSTextField::labelWithString(&ns(language_label()), mtm);
        lang_text.setFrame(NSRect::new(
            NSPoint::new(MARGIN, lang_bottom + (LANG_H - LABEL_H) / 2.0),
            NSSize::new(inner - LANG_W - 12.0, LABEL_H),
        ));
        lang_text.setFont(Some(&body));
        content.addSubview(&lang_text);
        let language = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(
                NSPoint::new(WIDTH - MARGIN - LANG_W, lang_bottom),
                NSSize::new(LANG_W, LANG_H),
            ),
            false,
        );
        for title in language_options() {
            language.addItemWithTitle(&ns(title));
        }
        language.selectItemAtIndex(language_index() as isize);
        content.addSubview(&language);
        y += LANG_H + 12.0;

        let button = |x: f64, y_top: f64, title: &str| {
            let b = NSButton::initWithFrame(
                NSButton::alloc(mtm),
                NSRect::new(
                    NSPoint::new(x, height - y_top - BUTTON_H),
                    NSSize::new(BUTTON_W, BUTTON_H),
                ),
            );
            b.setTitle(&ns(title));
            b.setBezelStyle(NSBezelStyle::Push);
            b
        };
        let save_x = WIDTH - MARGIN - BUTTON_W;
        let save_button = button(save_x, y, t("Save", "保存"));
        save_button.setKeyEquivalent(&ns("\r")); // Enter saves; also styles it as the default
        let test_button = button(
            save_x - 8.0 - BUTTON_W,
            y,
            t("Connection check", "接続チェック"),
        );
        content.addSubview(&test_button);
        content.addSubview(&save_button);
        y += BUTTON_H + 12.0;

        let result = NSTextField::wrappingLabelWithString(&ns(""), mtm);
        result.setFrame(rect(y, RESULT_H));
        result.setFont(Some(&small));
        result.setSelectable(true);
        content.addSubview(&result);

        let controller = Controller::new(
            mtm,
            inputs.clone(),
            language,
            result,
            test_button.clone(),
            saved_tx,
        );
        // An NSControl's target is a weak reference, so Window keeps the controller alive
        unsafe {
            test_button.setTarget(Some(&*controller));
            test_button.setAction(Some(sel!(onTest:)));
            save_button.setTarget(Some(&*controller));
            save_button.setAction(Some(sel!(onSave:)));
        }

        window.center();
        let this = Window {
            mtm,
            window,
            controller,
        };
        this.focus();
        if let Some((_, first)) = inputs.first() {
            this.window.makeFirstResponder(Some(first));
        }
        Ok(this)
    }

    /// Bring an already-open window to the front.
    pub fn focus(&self) {
        let app = NSApplication::sharedApplication(self.mtm);
        // The standard way for an Accessory (Dock-less) app to front its own window. The
        // successor activate() is sometimes ignored while another app is frontmost
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        self.window.makeKeyAndOrderFront(None);
    }

    /// Becomes false once the close button is used; the tray loop then drops the Window.
    pub fn is_open(&self) -> bool {
        self.window.isVisible()
    }

    /// Show the connection check result if one has arrived. The tray loop calls this every turn.
    pub fn poll(&mut self) {
        let ivars = self.controller.ivars();
        let outcome = {
            let mut slot = ivars.test_rx.borrow_mut();
            let Some(rx) = slot.as_ref() else { return };
            match rx.try_recv() {
                Err(mpsc::TryRecvError::Empty) => return,
                Ok(steps) => {
                    *slot = None;
                    Ok(steps)
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    *slot = None;
                    Err(())
                }
            }
        };
        ivars.test_button.setEnabled(true);
        match outcome {
            Ok(steps) => self.controller.set_result(&format_steps(&steps)),
            Err(()) => self.controller.set_result(&format!(
                "❌ {}",
                t(
                    "the connection-check thread died unexpectedly",
                    "接続チェックのスレッドが異常終了した"
                )
            )),
        }
    }
}

/// This process does not always follow the system light/dark appearance — that happens when it
/// is launched outside a proper bundle, from a terminal or launchd. Read AppleInterfaceStyle
/// from the system settings and match it explicitly every time a window opens.
fn sync_appearance(mtm: MainThreadMarker) {
    use objc2_app_kit::{NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua};
    use objc2_foundation::{NSUserDefaults, ns_string};
    let dark = NSUserDefaults::standardUserDefaults()
        .stringForKey(ns_string!("AppleInterfaceStyle"))
        .is_some_and(|style| style.to_string() == "Dark");
    let name = unsafe {
        if dark {
            NSAppearanceNameDarkAqua
        } else {
            NSAppearanceNameAqua
        }
    };
    if let Some(appearance) = NSAppearance::appearanceNamed(name) {
        NSApplication::sharedApplication(mtm).setAppearance(Some(&appearance));
    }
}

/// An Accessory app has no main menu, so editing keys such as Cmd+V do nothing as-is. Key
/// events are matched against the main menu's key equivalents, so register an edit menu once —
/// it never appears on screen, it just makes the shortcuts work.
fn ensure_edit_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    if app.mainMenu().is_some() {
        return;
    }
    let menubar = NSMenu::new(mtm);
    let item = NSMenuItem::new(mtm);
    menubar.addItem(&item);
    let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns(t("Edit", "編集")));
    unsafe {
        let _ = edit.addItemWithTitle_action_keyEquivalent(
            &ns(t("Undo", "取り消す")),
            Some(sel!(undo:)),
            &ns("z"),
        );
        let _ = edit.addItemWithTitle_action_keyEquivalent(
            &ns(t("Cut", "カット")),
            Some(sel!(cut:)),
            &ns("x"),
        );
        let _ = edit.addItemWithTitle_action_keyEquivalent(
            &ns(t("Copy", "コピー")),
            Some(sel!(copy:)),
            &ns("c"),
        );
        let _ = edit.addItemWithTitle_action_keyEquivalent(
            &ns(t("Paste", "ペースト")),
            Some(sel!(paste:)),
            &ns("v"),
        );
        let _ = edit.addItemWithTitle_action_keyEquivalent(
            &ns(t("Select All", "すべてを選択")),
            Some(sel!(selectAll:)),
            &ns("a"),
        );
    }
    item.setSubmenu(Some(&edit));
    app.setMainMenu(Some(&menubar));
}
