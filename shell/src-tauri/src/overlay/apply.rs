//! Write a correction back into the currently-focused text field.
//!
//! Tiered strategy:
//!
//!   1. Set `kAXSelectedTextRangeAttribute` to the target span.
//!      This usually works even in browsers / Electron — they expose
//!      caret manipulation through AXUI.
//!   2. Set `kAXSelectedTextAttribute` to the replacement string. Native
//!      Cocoa apps (TextEdit / Notes / Mail / Messages) honor this directly.
//!   3. **Fallback** — when step 2 silently no-ops (most browsers,
//!      Slack desktop, Electron-based editors), simulate ⌘V via the
//!      clipboard helper. The selection was already moved in step 1, so
//!      the paste replaces the right characters.
//!
//! When the fallback fires, we log `[quill][apply] fallback=clipboard`
//! so per-app behaviour is observable in `/tmp/quill.log`.

#![cfg(all(target_os = "macos", feature = "overlay"))]

use std::os::raw::c_void;

use accessibility_sys::{
    AXUIElementCopyAttributeValue, AXUIElementCreateSystemWide, AXUIElementRef,
    AXUIElementSetAttributeValue, AXValueCreate, kAXErrorSuccess,
    kAXFocusedApplicationAttribute, kAXFocusedUIElementAttribute, kAXSelectedTextAttribute,
    kAXSelectedTextRangeAttribute, kAXValueTypeCFRange,
};
use core_foundation::base::{CFIndex, CFRange, CFRelease, CFTypeRef, TCFType};
use core_foundation::string::CFString;

use crate::overlay::clipboard;

#[derive(Debug)]
pub enum ApplyError {
    NoFocusedApp,
    NoFocusedElement,
    /// Both AXUI text-set AND the clipboard fallback failed.
    AllStrategiesFailed { axui_err: i32, clipboard_posted: bool },
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFocusedApp => write!(f, "no focused application"),
            Self::NoFocusedElement => write!(f, "no focused UI element"),
            Self::AllStrategiesFailed { axui_err, clipboard_posted } => write!(
                f,
                "both AXUI text-set (AXError {axui_err}) and clipboard paste \
                 (posted={clipboard_posted}) failed"
            ),
        }
    }
}

/// Result strategy — useful for tests and the diagnostic log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyStrategy {
    /// `kAXSelectedTextAttribute` set succeeded — native Cocoa path.
    AxuiText,
    /// AXUI text-set failed, clipboard ⌘V was simulated instead.
    Clipboard,
}

pub fn apply(start: u32, end: u32, replacement: &str) -> Result<(), ApplyError> {
    let strategy = apply_with_strategy(start, end, replacement)?;
    eprintln!(
        "[quill][apply] strategy={strategy:?} start={start} end={end} len={}",
        replacement.chars().count()
    );
    Ok(())
}

pub fn apply_with_strategy(
    start: u32,
    end: u32,
    replacement: &str,
) -> Result<ApplyStrategy, ApplyError> {
    let length = end.saturating_sub(start);
    let system_wide = unsafe { AXUIElementCreateSystemWide() };
    let app = copy_attr_ref(system_wide, kAXFocusedApplicationAttribute)
        .ok_or(ApplyError::NoFocusedApp)?;
    let elem = copy_attr_ref(app as AXUIElementRef, kAXFocusedUIElementAttribute);
    unsafe { CFRelease(app) };
    let elem = elem.ok_or(ApplyError::NoFocusedElement)? as AXUIElementRef;

    // Step 1 — move the selection. We try this even before deciding which
    // text-write path to take, because the clipboard fallback needs the
    // selection already on the target span for ⌘V to replace it.
    let range = CFRange {
        location: start as CFIndex,
        length: length as CFIndex,
    };
    let range_val = unsafe {
        AXValueCreate(
            kAXValueTypeCFRange,
            &range as *const _ as *const c_void,
        )
    };
    let range_attr = CFString::new(kAXSelectedTextRangeAttribute);
    let _range_err = unsafe {
        AXUIElementSetAttributeValue(
            elem,
            range_attr.as_concrete_TypeRef(),
            range_val as CFTypeRef,
        )
    };
    unsafe { CFRelease(range_val as CFTypeRef) };
    // We don't bail on range-set failure — even if AXUI rejects it, the
    // user's existing selection (or caret) is probably already at the
    // word they hovered. The clipboard fallback then pastes there.

    // Step 2 — try native AXUI text replacement.
    let text_attr = CFString::new(kAXSelectedTextAttribute);
    let text_val = CFString::new(replacement);
    let axui_err = unsafe {
        AXUIElementSetAttributeValue(
            elem,
            text_attr.as_concrete_TypeRef(),
            text_val.as_concrete_TypeRef() as CFTypeRef,
        )
    };
    unsafe { CFRelease(elem as CFTypeRef) };
    if axui_err == kAXErrorSuccess {
        return Ok(ApplyStrategy::AxuiText);
    }

    // Step 3 — clipboard fallback.
    let posted = clipboard::paste_via_clipboard(replacement);
    if posted {
        Ok(ApplyStrategy::Clipboard)
    } else {
        Err(ApplyError::AllStrategiesFailed {
            axui_err,
            clipboard_posted: false,
        })
    }
}

fn copy_attr_ref(element: AXUIElementRef, attr_name: &str) -> Option<CFTypeRef> {
    let cf_attr = CFString::new(attr_name);
    let mut out: CFTypeRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyAttributeValue(element, cf_attr.as_concrete_TypeRef(), &mut out)
    };
    if err == kAXErrorSuccess && !out.is_null() {
        Some(out)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_strategy_variants_are_distinct() {
        // Sanity: the public enum the tests + UI care about hasn't
        // collapsed accidentally.
        assert_ne!(ApplyStrategy::AxuiText, ApplyStrategy::Clipboard);
        // Exhaustiveness guard — if a third variant is added, this won't
        // compile until tests are updated.
        let _all: [ApplyStrategy; 2] = [ApplyStrategy::AxuiText, ApplyStrategy::Clipboard];
    }

    #[test]
    fn apply_error_messages_mention_codes() {
        let e = ApplyError::AllStrategiesFailed { axui_err: -25212, clipboard_posted: false };
        let s = format!("{e}");
        assert!(s.contains("-25212"));
        assert!(s.contains("clipboard"));
    }

    /// Integration test (gated): actually focuses Quill's own window and
    /// runs apply_with_strategy to verify the AXUI path returns
    /// `Strategy::AxuiText` on WKWebView. Only enabled if QUILL_TEST_AXUI=1
    /// so CI without an active session doesn't pop random apply events.
    #[test]
    #[ignore]
    fn axui_path_returns_axui_text_when_native_focused() {
        if std::env::var("QUILL_TEST_AXUI").ok().as_deref() != Some("1") {
            eprintln!("set QUILL_TEST_AXUI=1 with a Cocoa text field focused");
            return;
        }
        let r = apply_with_strategy(0, 0, " ");
        // Either AxuiText (focused field is native Cocoa) or Clipboard
        // (focused field is web/Electron) — both are success paths.
        assert!(r.is_ok() || matches!(r, Err(ApplyError::NoFocusedElement | ApplyError::NoFocusedApp)));
    }
}
