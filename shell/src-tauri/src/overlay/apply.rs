//! Write a correction back into the currently-focused text field via AXUI.
//!
//! Sets `kAXSelectedTextRangeAttribute` to the target span, then sets
//! `kAXSelectedTextAttribute` to the replacement string. Cocoa text views
//! and most native fields honor this. Web inputs / Electron typically don't.

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

#[derive(Debug)]
pub enum ApplyError {
    NoFocusedApp,
    NoFocusedElement,
    RangeSetFailed(i32),
    TextSetFailed(i32),
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoFocusedApp => write!(f, "no focused application"),
            Self::NoFocusedElement => write!(f, "no focused UI element"),
            Self::RangeSetFailed(c) => write!(f, "setting selected range failed (AXError {c})"),
            Self::TextSetFailed(c) => write!(f, "setting selected text failed (AXError {c})"),
        }
    }
}

pub fn apply(start: u32, end: u32, replacement: &str) -> Result<(), ApplyError> {
    let length = end.saturating_sub(start);
    let system_wide = unsafe { AXUIElementCreateSystemWide() };
    let app = copy_attr_ref(system_wide, kAXFocusedApplicationAttribute)
        .ok_or(ApplyError::NoFocusedApp)?;
    let elem = copy_attr_ref(app as AXUIElementRef, kAXFocusedUIElementAttribute);
    unsafe { CFRelease(app) };
    let elem = elem.ok_or(ApplyError::NoFocusedElement)? as AXUIElementRef;

    // 1. Move the selection to the span we want to replace.
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
    let err1 = unsafe {
        AXUIElementSetAttributeValue(
            elem,
            range_attr.as_concrete_TypeRef(),
            range_val as CFTypeRef,
        )
    };
    unsafe { CFRelease(range_val as CFTypeRef) };
    if err1 != kAXErrorSuccess {
        unsafe { CFRelease(elem as CFTypeRef) };
        return Err(ApplyError::RangeSetFailed(err1));
    }

    // 2. Replace the now-selected text with the suggestion.
    let text_attr = CFString::new(kAXSelectedTextAttribute);
    let text_val = CFString::new(replacement);
    let err2 = unsafe {
        AXUIElementSetAttributeValue(
            elem,
            text_attr.as_concrete_TypeRef(),
            text_val.as_concrete_TypeRef() as CFTypeRef,
        )
    };
    unsafe { CFRelease(elem as CFTypeRef) };
    if err2 != kAXErrorSuccess {
        return Err(ApplyError::TextSetFailed(err2));
    }

    Ok(())
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
