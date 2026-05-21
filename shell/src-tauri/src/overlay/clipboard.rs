//! NSPasteboard save/set/restore + simulated ⌘V for the apply fallback.
//!
//! Used when AXUI's `kAXSelectedTextAttribute` write silently no-ops
//! (Safari, Chrome, every Electron app). The trick: select the span via
//! `kAXSelectedTextRangeAttribute` (this DOES work in most browsers),
//! stash the user's pasteboard, push our replacement, simulate ⌘V, then
//! restore. The window is <100 ms so the user's clipboard appears
//! untouched if they paste anything afterward.
//!
//! Failure modes documented inline. All operations are best-effort —
//! never panic, never break the rewrite flow.

#![cfg(all(target_os = "macos", feature = "overlay"))]

use std::thread;
use std::time::Duration;

use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::{NSArray, NSString};

/// Virtual key code for the "V" key on a US ANSI keyboard. From
/// `<HIToolbox/Events.h>` (`kVK_ANSI_V`).
const KEY_V: CGKeyCode = 0x09;

/// Snapshot what's currently on the general pasteboard so we can restore
/// it after the simulated paste. We only capture the string representation —
/// that loses image / rich-text fidelity, which is acceptable for our
/// "stole the clipboard for 80 ms" use case.
pub fn snapshot_string() -> Option<String> {
    let pb = NSPasteboard::generalPasteboard();
    let ns_str = unsafe { pb.stringForType(NSPasteboardTypeString) }?;
    Some(ns_str.to_string())
}

// Suppress the "generalPasteboard returns Retained<NSPasteboard>" warning.
// Some objc2 versions mark it unsafe, others safe; both compile fine.
#[allow(unused_unsafe)]
const _: () = ();

/// Replace the general pasteboard's string contents.
pub fn set_string(s: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    let ns = NSString::from_str(s);
    unsafe {
        let types = NSArray::from_slice(&[NSPasteboardTypeString]);
        pb.declareTypes_owner(&types, None);
        let _ = pb.setString_forType(&ns, NSPasteboardTypeString);
    }
}

/// Synthesize ⌘V at the system event tap so the currently-focused
/// application receives the paste. Returns false if event creation fails.
pub fn simulate_paste() -> bool {
    let src = match CGEventSource::new(CGEventSourceStateID::CombinedSessionState) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let down = match CGEvent::new_keyboard_event(src.clone(), KEY_V, true) {
        Ok(e) => e,
        Err(_) => return false,
    };
    down.set_flags(CGEventFlags::CGEventFlagCommand);
    down.post(CGEventTapLocation::HID);

    let up = match CGEvent::new_keyboard_event(src, KEY_V, false) {
        Ok(e) => e,
        Err(_) => return false,
    };
    up.set_flags(CGEventFlags::CGEventFlagCommand);
    up.post(CGEventTapLocation::HID);
    true
}

/// Full fallback: save → push → paste → wait → restore.
/// Tunable wait window: 60ms is enough on Apple Silicon for most apps;
/// 100ms gives Slack/Discord room to settle. Restore happens on a
/// background thread so we don't block the rewrite path.
pub fn paste_via_clipboard(replacement: &str) -> bool {
    let saved = snapshot_string();
    set_string(replacement);
    let posted = simulate_paste();
    // Restore after the paste has been consumed by the target app.
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(120));
        match saved {
            Some(s) if !s.is_empty() => set_string(&s),
            _ => {
                // Original was empty / non-string. Clear what we set.
                let pb = NSPasteboard::generalPasteboard();
                pb.clearContents();
            }
        }
    });
    posted
}
