//! macOS-only system-wide focus tracker + overlay window setup.
//! Compiled only when `feature = "overlay"` AND `target_os = "macos"`.

#![cfg(all(target_os = "macos", feature = "overlay"))]

pub mod apply;
pub mod focus_tracker;
pub mod mouse_arbiter;
pub mod window;
