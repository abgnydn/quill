//! Creates the always-on-top, click-through overlay window that renders a
//! border around the focused text element.

#![cfg(all(target_os = "macos", feature = "overlay"))]

use tauri::{AppHandle, WebviewUrl, WebviewWindowBuilder};

/// Spawn the overlay window. It covers the main display, is transparent,
/// click-through (`set_ignore_cursor_events`), and floats above other apps.
/// Frontend at `src/overlay.html` listens for `focus-update` events and
/// positions a `<div>` border at the reported bounds.
pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let win = WebviewWindowBuilder::new(app, "overlay", WebviewUrl::App("overlay.html".into()))
        .title("Quill Overlay")
        .inner_size(4096.0, 3072.0)
        .position(0.0, 0.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .resizable(false)
        .skip_taskbar(true)
        .shadow(false)
        .focused(false)
        .build()?;

    win.set_ignore_cursor_events(true)?;

    // Apply macOS-specific window behaviors so the overlay floats above
    // fullscreen apps and stays out of Cmd-Tab / Mission Control.
    apply_macos_window_styling(&win);

    Ok(())
}

fn apply_macos_window_styling(win: &tauri::WebviewWindow) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let Ok(raw) = win.ns_window() else { return };
    let ns_window = raw as *mut AnyObject;
    if ns_window.is_null() {
        return;
    }

    // Levels and collection-behavior constants pulled from <AppKit/NSWindow.h>.
    // kCGMaximumWindowLevel ≈ NSScreenSaverWindowLevel keeps us above almost
    // everything except the menubar's own screen-shot HUD.
    const NS_SCREEN_SAVER_LEVEL: i64 = 1000;
    // canJoinAllSpaces | fullScreenAuxiliary | stationary | ignoresCycle
    const COLLECTION: u64 = (1 << 0) | (1 << 8) | (1 << 4) | (1 << 6);

    unsafe {
        let _: () = msg_send![ns_window, setLevel: NS_SCREEN_SAVER_LEVEL];
        let _: () = msg_send![ns_window, setCollectionBehavior: COLLECTION];
        let _: () = msg_send![ns_window, setHasShadow: false];
    }
}
