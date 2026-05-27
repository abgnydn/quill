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
    AXUIElementCopyParameterizedAttributeValue, AXUIElementCreateSystemWide, AXUIElementGetPid,
    AXUIElementRef, AXValueCreate, AXValueGetValue, AXValueRef,
    kAXBoundsForRangeParameterizedAttribute, kAXErrorSuccess, kAXFocusedApplicationAttribute,
    kAXFocusedUIElementAttribute, kAXPositionAttribute, kAXRoleAttribute,
    kAXRoleDescriptionAttribute, kAXSizeAttribute, kAXSubroleAttribute,
    kAXTrustedCheckOptionPrompt, kAXValueAttribute, kAXValueTypeCFRange, kAXValueTypeCGPoint,
    kAXValueTypeCGRect, kAXValueTypeCGSize,
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
    pub lint: crate::wire::WireLint,
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
pub fn spawn(app: AppHandle, config: std::sync::Arc<crate::config::ConfigStore>) {
    thread::Builder::new()
        .name("quill-focus-tracker".into())
        .spawn(move || run(app, config))
        .expect("spawn focus-tracker thread");
}

/// Build a private LintGroup for the tracker thread. Harper's `concurrent`
/// feature is enabled in Cargo.toml so the dictionary is `Send`. Mirrors
/// `state::build_linter` — both call sites must enable the same extra rules
/// so the main-window panel and the overlay surface identical lints.
fn fresh_linter() -> harper_core::linting::LintGroup {
    crate::state::build_linter()
}

fn run(app: AppHandle, config: std::sync::Arc<crate::config::ConfigStore>) {
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
    let mut last_skip: Option<SkipContext> = None;
    let mut tick = 0u32;

    let mut last_paused_state: Option<bool> = None;
    loop {
        thread::sleep(Duration::from_millis(150));
        tick = tick.wrapping_add(1);

        // Pause short-circuit — when the user toggled Quill paused (via tray
        // or settings), we skip every AXUI read.
        let cfg = config.snapshot();
        if cfg.paused {
            if last_paused_state != Some(true) {
                eprintln!("[quill] paused — overlay silent until resumed");
                last_paused_state = Some(true);
                last_bounds = None;
                last_text_hash = 0;
                last_skip = None;
                // Clear saved element so a stale handle doesn't outlive pause.
                crate::overlay::engaged_elem::clear();
                // Tell the overlay to hide.
                let _ = app.emit_to(OVERLAY_LABEL, "focus-update", &FocusEvent {
                    bounds: None, text: None, lints: vec![],
                });
            }
            continue;
        } else if last_paused_state == Some(true) {
            eprintln!("[quill] resumed");
            last_paused_state = Some(false);
        }

        let snapshot = focused_snapshot(system_wide, &cfg);

        // Log on every NEW skip context — gives users visible evidence that
        // the engagement filter is working without spamming on every poll.
        //
        // SPECIAL CASE: when the focused app is Nib itself (the user clicked
        // our overlay popover, which activated us), suppress the focus
        // update entirely. Otherwise the empty event clears currentLints in
        // JS and leaves stale underlines on screen when the user later
        // switches to a different app. The cached engaged_elem keeps apply
        // pointing at the right text field.
        if let SnapshotResult::Skip(ctx) = &snapshot {
            if last_skip.as_ref() != Some(ctx) {
                eprintln!(
                    "[quill] focus skipped: bundle={:?} role={:?} subrole={:?} role_desc={:?}",
                    ctx.bundle_id, ctx.role, ctx.subrole, ctx.role_description
                );
                last_skip = Some(ctx.clone());
            }
            if ctx.bundle_id.as_deref() == Some("app.nib") {
                // Don't propagate — let JS keep its prior focused-field state.
                continue;
            }
        } else {
            last_skip = None;
        }

        let snap_opt = match snapshot {
            SnapshotResult::Engage(s) => Some(s),
            SnapshotResult::Skip(_) | SnapshotResult::Empty => None,
        };
        let bounds_changed = snap_opt.as_ref().map(|s| &s.bounds) != last_bounds.as_ref();
        let text_hash = snap_opt
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

        let (bounds, text, elem_ref) = match snap_opt {
            Some(s) => (Some(s.bounds), s.text, s.elem),
            None => (None, None, std::ptr::null_mut()),
        };

        // Lint the text if we got any. Harper takes ~5-30ms per check on
        // typical sentence-length input. Honor the user's personal
        // dictionary so e.g. "BitNet" or "abgunaydin" doesn't get flagged.
        let raw_lints: Vec<crate::wire::WireLint> = match &text {
            Some(t) if !t.is_empty() => {
                crate::wire::check_text_filtered(&mut linter, t, &cfg.ignored_words)
            }
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

        // Transfer the AXUIElement handle into the shared engaged-elem cache
        // so apply.rs can write to it even after the user clicks our overlay
        // (which would shift live AXUI focus away from their text field).
        // store() takes ownership of the retain — no CFRelease here.
        if !elem_ref.is_null() {
            crate::overlay::engaged_elem::store(elem_ref as *mut std::ffi::c_void);
        }

        let rects_resolved = lints.iter().filter(|l| l.rect.is_some()).count();
        eprintln!(
            "[quill] focus-update: bounds={} text_len={} lints={} rects={}/{}",
            bounds
                .as_ref()
                .map(|b| format!("x={:.0} y={:.0} w={:.0} h={:.0}", b.x, b.y, b.w, b.h))
                .unwrap_or_else(|| "<none>".into()),
            text.as_deref().map(|t| t.chars().count()).unwrap_or(0),
            lints.len(),
            rects_resolved,
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

/// Identifies a focus context the engagement policy rejected. The run loop
/// uses this for log-on-change diagnostics so users can see *why* an app
/// was skipped without spamming the log on every poll.
#[derive(Clone, PartialEq, Debug)]
pub struct SkipContext {
    pub bundle_id: Option<String>,
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub role_description: Option<String>,
}

enum SnapshotResult {
    Engage(FocusSnapshot),
    Skip(SkipContext),
    Empty,
}

fn focused_snapshot(
    system_wide: AXUIElementRef,
    config_snap: &crate::config::Config,
) -> SnapshotResult {
    let Some(focused_app) = copy_attr(system_wide, kAXFocusedApplicationAttribute) else {
        return SnapshotResult::Empty;
    };
    let bundle_id = bundle_id_for_app(focused_app as AXUIElementRef);
    let focused_elem = copy_attr(focused_app as AXUIElementRef, kAXFocusedUIElementAttribute);
    unsafe { CFRelease(focused_app) };
    let Some(focused_elem) = focused_elem else {
        return SnapshotResult::Empty;
    };
    let elem_ref = focused_elem as AXUIElementRef;

    let role = copy_string_attr(elem_ref, kAXRoleAttribute);
    let subrole = copy_string_attr(elem_ref, kAXSubroleAttribute);
    let role_description = copy_string_attr(elem_ref, kAXRoleDescriptionAttribute);

    // Per-app override (set via Settings UI). ForceDeny always skips;
    // ForceAllow bypasses the engagement policy entirely.
    let user_override = bundle_id
        .as_deref()
        .and_then(|bid| config_snap.app_override(bid));
    let engage = match user_override {
        Some(crate::config::AppOverride::ForceDeny) => false,
        Some(crate::config::AppOverride::ForceAllow) => true,
        None => crate::overlay::engagement_policy::is_engageable(
            role.as_deref(),
            subrole.as_deref(),
            role_description.as_deref(),
            bundle_id.as_deref(),
        ),
    };
    if !engage {
        unsafe { CFRelease(focused_elem) };
        return SnapshotResult::Skip(SkipContext { bundle_id, role, subrole, role_description });
    }

    let pos = copy_axvalue_cgpoint(elem_ref, kAXPositionAttribute);
    let size = copy_axvalue_cgsize(elem_ref, kAXSizeAttribute);
    let text = copy_string_attr(elem_ref, kAXValueAttribute);
    // NOTE: we don't release focused_elem here — caller takes ownership and
    // releases after using it for per-lint bounds lookups.

    let (p, s) = match (pos, size) {
        (Some(p), Some(s)) => (p, s),
        _ => {
            unsafe { CFRelease(focused_elem) };
            return SnapshotResult::Empty;
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
        return SnapshotResult::Empty;
    }
    SnapshotResult::Engage(FocusSnapshot {
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

/// Resolve the bundle identifier of the app behind an AX application
/// element via pid → NSRunningApplication. Returns None for processes
/// without a registered bundle (rare; daemons, helper procs).
fn bundle_id_for_app(app_elem: AXUIElementRef) -> Option<String> {
    use objc2_app_kit::NSRunningApplication;
    // pid_t is i32 on macOS; we're cfg-gated to macOS only.
    let mut pid: i32 = 0;
    let err = unsafe { AXUIElementGetPid(app_elem, &mut pid) };
    if err != kAXErrorSuccess || pid <= 0 {
        return None;
    }
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    let s = app.bundleIdentifier()?;
    Some(s.to_string())
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
