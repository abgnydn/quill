//! Polls the macOS Accessibility API at 10 Hz for the focused UI element's
//! screen bounds and emits Tauri `focus-update` events to the overlay window.
//!
//! Requires Accessibility permission. First launch will prompt; user must grant
//! in System Settings → Privacy & Security → Accessibility.

#![cfg(all(target_os = "macos", feature = "overlay"))]

use std::thread;
use std::time::Duration;

use accessibility_sys::{
    AXIsProcessTrustedWithOptions, AXUIElementCopyAttributeValue,
    AXUIElementCopyParameterizedAttributeValue, AXUIElementCreateSystemWide, AXUIElementRef,
    AXValueCreate, AXValueGetValue, AXValueRef, kAXBoundsForRangeParameterizedAttribute,
    kAXErrorSuccess, kAXFocusedApplicationAttribute, kAXFocusedUIElementAttribute,
    kAXPositionAttribute, kAXSizeAttribute, kAXTrustedCheckOptionPrompt, kAXValueAttribute,
    kAXValueTypeCFRange, kAXValueTypeCGPoint, kAXValueTypeCGRect, kAXValueTypeCGSize,
};
use core_foundation::base::{CFIndex, CFRange, CFType};
use core_graphics::geometry::CGRect;
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

/// A WireLint plus the precomputed screen rect for its character span.
/// `rect` is `None` if AXUI's `kAXBoundsForRangeParameterizedAttribute` failed
/// for this element (common in web text inputs).
#[derive(Serialize, Clone, Debug)]
pub struct PositionedLint {
    #[serde(flatten)]
    pub lint: crate::WireLint,
    pub rect: Option<FocusBounds>,
}

#[derive(Serialize, Clone, Debug)]
pub struct FocusEvent {
    pub bounds: Option<FocusBounds>,
    pub text: Option<String>,
    pub lints: Vec<PositionedLint>,
}

/// Spawn the polling thread. Returns immediately; logs Accessibility-permission
/// status to stderr.
pub fn spawn(app: AppHandle) {
    thread::Builder::new()
        .name("quill-focus-tracker".into())
        .spawn(move || run(app))
        .expect("spawn focus-tracker thread");
}

/// Build a private LintGroup for the tracker thread. Harper's `concurrent`
/// feature is enabled in Cargo.toml so the dictionary is `Send`.
fn fresh_linter() -> harper_core::linting::LintGroup {
    use harper_core::Dialect;
    use harper_core::linting::LintGroup;
    use harper_core::spell::FstDictionary;
    LintGroup::new_curated(FstDictionary::curated(), Dialect::American)
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
    let mut linter = fresh_linter();
    let mut last_bounds: Option<FocusBounds> = None;
    let mut last_text_hash: u64 = 0;
    let mut tick = 0u32;

    loop {
        thread::sleep(Duration::from_millis(150));
        tick = tick.wrapping_add(1);

        let snapshot = focused_snapshot(system_wide);
        let bounds_changed = snapshot.as_ref().map(|s| &s.bounds) != last_bounds.as_ref();
        let text_hash = snapshot
            .as_ref()
            .and_then(|s| s.text.as_deref())
            .map(simple_hash)
            .unwrap_or(0);
        let text_changed = text_hash != last_text_hash;

        if !bounds_changed && !text_changed {
            if tick % 60 == 0 {
                eprintln!("[quill] focus-tracker heartbeat (no change in 9s)");
            }
            continue;
        }

        let (bounds, text, elem_ref) = match snapshot {
            Some(s) => (Some(s.bounds), s.text, s.elem),
            None => (None, None, std::ptr::null_mut()),
        };

        // Lint the text if we got any. Harper takes ~5-30ms per check on
        // typical sentence-length input.
        let raw_lints: Vec<crate::WireLint> = match &text {
            Some(t) if !t.is_empty() => crate::check_text_with(&mut linter, t),
            _ => Vec::new(),
        };

        // For each lint, ask AXUI where on the screen those characters sit.
        // Many text fields (Cocoa NSTextView, AppKit) implement
        // kAXBoundsForRangeParameterizedAttribute; web text inputs often don't.
        let lints: Vec<PositionedLint> = raw_lints
            .into_iter()
            .map(|lint| {
                let rect = if !elem_ref.is_null() {
                    bounds_for_range(elem_ref, lint.start, lint.end - lint.start)
                } else {
                    None
                };
                PositionedLint { lint, rect }
            })
            .collect();

        if !elem_ref.is_null() {
            unsafe { CFRelease(elem_ref as core_foundation::base::CFTypeRef) };
        }

        eprintln!(
            "[quill] focus-update: bounds={} text_len={} lints={}",
            bounds
                .as_ref()
                .map(|b| format!("x={:.0} y={:.0} w={:.0} h={:.0}", b.x, b.y, b.w, b.h))
                .unwrap_or_else(|| "<none>".into()),
            text.as_deref().map(|t| t.chars().count()).unwrap_or(0),
            lints.len()
        );

        let payload = FocusEvent {
            bounds: bounds.clone(),
            text: text.clone(),
            lints,
        };
        if let Err(e) = app.emit_to(OVERLAY_LABEL, "focus-update", &payload) {
            eprintln!("[quill] emit_to overlay failed: {e}");
        }
        let _ = app.emit("focus-update", &payload);

        last_bounds = bounds;
        last_text_hash = text_hash;
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

struct FocusSnapshot {
    bounds: FocusBounds,
    text: Option<String>,
    /// Borrowed AXUIElementRef — caller must CFRelease when done with it.
    elem: AXUIElementRef,
}

fn focused_snapshot(system_wide: AXUIElementRef) -> Option<FocusSnapshot> {
    let focused_app = copy_attr(system_wide, kAXFocusedApplicationAttribute)?;
    let focused_elem = copy_attr(focused_app as AXUIElementRef, kAXFocusedUIElementAttribute);
    unsafe { CFRelease(focused_app) };
    let focused_elem = focused_elem?;
    let elem_ref = focused_elem as AXUIElementRef;

    let pos = copy_axvalue_cgpoint(elem_ref, kAXPositionAttribute);
    let size = copy_axvalue_cgsize(elem_ref, kAXSizeAttribute);
    let text = copy_string_attr(elem_ref, kAXValueAttribute);
    // NOTE: we don't release focused_elem here — caller takes ownership and
    // releases after using it for per-lint bounds lookups.

    let (p, s) = match (pos, size) {
        (Some(p), Some(s)) => (p, s),
        _ => {
            unsafe { CFRelease(focused_elem) };
            return None;
        }
    };
    let b = FocusBounds {
        x: p.x,
        y: p.y,
        w: s.width,
        h: s.height,
    };
    if !is_plausible_text_field(&b) {
        unsafe { CFRelease(focused_elem) };
        return None;
    }
    Some(FocusSnapshot {
        bounds: b,
        text,
        elem: elem_ref,
    })
}

/// Ask AXUI: where on the screen does character range [start..start+length)
/// of this element sit? Used for inline underline rendering.
pub fn bounds_for_range(
    element: AXUIElementRef,
    start: usize,
    length: usize,
) -> Option<FocusBounds> {
    if length == 0 {
        return None;
    }
    let range = CFRange {
        location: start as CFIndex,
        length: length as CFIndex,
    };
    let range_val: AXValueRef = unsafe {
        AXValueCreate(
            kAXValueTypeCFRange,
            &range as *const _ as *const std::ffi::c_void,
        )
    };
    if range_val.is_null() {
        return None;
    }

    let attr_cf = CFString::new(kAXBoundsForRangeParameterizedAttribute);
    let mut out: core_foundation::base::CFTypeRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyParameterizedAttributeValue(
            element,
            attr_cf.as_concrete_TypeRef(),
            range_val as core_foundation::base::CFTypeRef,
            &mut out,
        )
    };
    unsafe { CFRelease(range_val as core_foundation::base::CFTypeRef) };
    if err != kAXErrorSuccess || out.is_null() {
        return None;
    }
    let mut rect = CGRect::new(
        &core_graphics::geometry::CGPoint::new(0.0, 0.0),
        &core_graphics::geometry::CGSize::new(0.0, 0.0),
    );
    let ok = unsafe {
        AXValueGetValue(
            out as AXValueRef,
            kAXValueTypeCGRect,
            &mut rect as *mut _ as *mut std::ffi::c_void,
        )
    };
    unsafe { CFRelease(out) };
    if !ok {
        return None;
    }
    Some(FocusBounds {
        x: rect.origin.x,
        y: rect.origin.y,
        w: rect.size.width,
        h: rect.size.height,
    })
}

/// Read a CFString-valued attribute off the focused element. Returns None
/// for non-text elements (the attribute is wrong type or absent).
fn copy_string_attr(element: AXUIElementRef, attr_name: &str) -> Option<String> {
    let raw = copy_attr(element, attr_name)?;
    // Verify it's actually a CFString before unsafe-wrapping.
    let cf_any = unsafe { CFType::wrap_under_create_rule(raw) };
    cf_any
        .downcast::<CFString>()
        .map(|s| s.to_string())
}

/// Cheap hash for change detection on potentially-large text snapshots.
fn simple_hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
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
