//! Polls the macOS Accessibility API at 10 Hz for the focused UI element's
//! screen bounds and emits Tauri `focus-update` events to the overlay window.
//!
//! Requires Accessibility permission. First launch will prompt; user must grant
//! in System Settings → Privacy & Security → Accessibility.

#![cfg(all(target_os = "macos", feature = "overlay"))]

use std::thread;
use std::time::Duration;

use accessibility_sys::{
    AXIsProcessTrustedWithOptions, AXUIElementCopyAttributeValue, AXUIElementCreateSystemWide,
    AXUIElementRef, AXValueGetValue, AXValueRef, kAXErrorSuccess,
    kAXFocusedApplicationAttribute, kAXFocusedUIElementAttribute, kAXPositionAttribute,
    kAXSizeAttribute, kAXTrustedCheckOptionPrompt, kAXValueTypeCGPoint, kAXValueTypeCGSize,
};
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::geometry::{CGPoint, CGSize};
use serde::Serialize;
use tauri::{AppHandle, Emitter};
const OVERLAY_LABEL: &str = "overlay";

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct FocusBounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_text_field_bounds_pass() {
        assert!(is_plausible_text_field(&FocusBounds { x: 418.0, y: 239.0, w: 521.0, h: 497.0 }));
        assert!(is_plausible_text_field(&FocusBounds { x: 0.0, y: 0.0, w: 200.0, h: 24.0 }));
    }

    #[test]
    fn axui_garbage_rejected() {
        // Real example seen in the wild — outer scrollview reporting itself.
        assert!(!is_plausible_text_field(&FocusBounds { x: -1.0, y: -17899.0, w: 1711.0, h: 19017.0 }));
        // Tiny zero-sized element (e.g., empty label)
        assert!(!is_plausible_text_field(&FocusBounds { x: 0.0, y: 0.0, w: 4.0, h: 4.0 }));
        // Absurdly tall column
        assert!(!is_plausible_text_field(&FocusBounds { x: 0.0, y: 0.0, w: 200.0, h: 10_000.0 }));
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct FocusEvent {
    pub bounds: Option<FocusBounds>,
}

/// Spawn the polling thread. Returns immediately; logs Accessibility-permission
/// status to stderr.
pub fn spawn(app: AppHandle) {
    thread::Builder::new()
        .name("quill-focus-tracker".into())
        .spawn(move || run(app))
        .expect("spawn focus-tracker thread");
}

fn run(app: AppHandle) {
    // First check WITH prompt: triggers the system "grant Accessibility?"
    // dialog the first time. AXIsProcessTrustedWithOptions does a fresh read
    // each call (unlike AXIsProcessTrusted, which caches per-process).
    if !is_trusted(true) {
        eprintln!(
            "[quill] Accessibility permission NOT granted. \
             System Settings → Privacy & Security → Accessibility — toggle Quill on."
        );
        // Poll every 2s using the fresh-read variant — picks up the grant
        // without requiring a relaunch.
        while !is_trusted(false) {
            thread::sleep(Duration::from_secs(2));
        }
        eprintln!("[quill] Accessibility permission granted; focus tracker starting");
    } else {
        eprintln!("[quill] AXUI trusted; focus tracker starting");
    }

    let system_wide = unsafe { AXUIElementCreateSystemWide() };
    let mut last: Option<FocusBounds> = None;
    let mut tick = 0u32;

    loop {
        thread::sleep(Duration::from_millis(100));
        tick = tick.wrapping_add(1);

        let bounds = focused_bounds(system_wide);
        if bounds != last {
            eprintln!(
                "[quill] focus-update: {:?}",
                bounds.as_ref().map(|b| format!("x={:.0} y={:.0} w={:.0} h={:.0}", b.x, b.y, b.w, b.h))
                    .unwrap_or_else(|| "<none>".into())
            );
            let payload = FocusEvent { bounds: bounds.clone() };
            // Send to the overlay window specifically. The global `emit` is
            // supposed to broadcast but we saw events not reaching the overlay
            // webview in practice — `emit_to` is the reliable form.
            if let Err(e) = app.emit_to(OVERLAY_LABEL, "focus-update", &payload) {
                eprintln!("[quill] emit_to overlay failed: {e}");
            }
            let _ = app.emit("focus-update", &payload);
            last = bounds;
        } else if tick % 50 == 0 {
            eprintln!("[quill] focus-tracker heartbeat (no change in last 5s)");
        }
    }
}

/// Fresh-read trust check. `prompt=true` shows the system dialog if not
/// already granted (use sparingly — only on startup).
fn is_trusted(prompt: bool) -> bool {
    let key = unsafe { CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt as CFStringRef) };
    let val = CFBoolean::from(prompt);
    let opts = CFDictionary::from_CFType_pairs(&[(key, val)]);
    unsafe { AXIsProcessTrustedWithOptions(opts.as_concrete_TypeRef() as _) }
}

fn focused_bounds(system_wide: AXUIElementRef) -> Option<FocusBounds> {
    let focused_app = copy_attr(system_wide, kAXFocusedApplicationAttribute)?;
    let focused_elem = copy_attr(focused_app as AXUIElementRef, kAXFocusedUIElementAttribute);
    unsafe { CFRelease(focused_app) };
    let focused_elem = focused_elem?;

    let pos = copy_axvalue_cgpoint(focused_elem as AXUIElementRef, kAXPositionAttribute);
    let size = copy_axvalue_cgsize(focused_elem as AXUIElementRef, kAXSizeAttribute);
    unsafe { CFRelease(focused_elem) };

    let (p, s) = (pos?, size?);
    let b = FocusBounds {
        x: p.x,
        y: p.y,
        w: s.width,
        h: s.height,
    };
    if !is_plausible_text_field(&b) {
        return None;
    }
    Some(b)
}

/// Reject AXUI bounds that obviously aren't a real text input — outer
/// scrollviews and window background elements like to report
/// `x=-1 y=-17899 w=1711 h=19017` (giant rectangle stretching off-screen).
fn is_plausible_text_field(b: &FocusBounds) -> bool {
    // Reasonable on-screen text field is somewhere between 16 and 4000 px in
    // both dimensions, and its top-left isn't ridiculously off-screen.
    let on_screen_y = b.y > -1000.0 && b.y < 8000.0;
    let on_screen_x = b.x > -1000.0 && b.x < 8000.0;
    let sane_w = b.w >= 16.0 && b.w <= 4000.0;
    let sane_h = b.h >= 8.0 && b.h <= 4000.0;
    on_screen_x && on_screen_y && sane_w && sane_h
}

/// Copy an attribute and return the raw CFTypeRef (caller MUST release).
/// Returns None on any AX error.
fn copy_attr(element: AXUIElementRef, attr_name: &str) -> Option<CFTypeRef> {
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

fn copy_axvalue_cgpoint(element: AXUIElementRef, attr: &str) -> Option<CGPoint> {
    let val = copy_attr(element, attr)? as AXValueRef;
    let mut p = CGPoint { x: 0.0, y: 0.0 };
    let ok = unsafe {
        AXValueGetValue(
            val,
            kAXValueTypeCGPoint,
            &mut p as *mut _ as *mut std::ffi::c_void,
        )
    };
    unsafe { CFRelease(val as CFTypeRef) };
    if ok { Some(p) } else { None }
}

fn copy_axvalue_cgsize(element: AXUIElementRef, attr: &str) -> Option<CGSize> {
    let val = copy_attr(element, attr)? as AXValueRef;
    let mut s = CGSize { width: 0.0, height: 0.0 };
    let ok = unsafe {
        AXValueGetValue(
            val,
            kAXValueTypeCGSize,
            &mut s as *mut _ as *mut std::ffi::c_void,
        )
    };
    unsafe { CFRelease(val as CFTypeRef) };
    if ok { Some(s) } else { None }
}
