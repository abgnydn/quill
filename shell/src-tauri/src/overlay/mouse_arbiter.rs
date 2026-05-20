//! Cursor-position arbiter for the click-through overlay.
//!
//! The overlay window is `ignoresMouseEvents = true` by default so clicks
//! pass through to whatever app the user is using. But when the cursor is
//! inside one of our interactive "hot regions" (an underline, a hover
//! popover) we flip `ignoresMouseEvents = false` so the webview gets the
//! mouse events — then flip back when the cursor leaves.
//!
//! Hot regions are pushed from JS via the `overlay_set_hot_regions` Tauri
//! command. The arbiter polls the cursor location at 40 Hz via Quartz.

#![cfg(all(target_os = "macos", feature = "overlay"))]

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use core_graphics::event::CGEvent;
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Clone, Debug, Deserialize)]
pub struct HotRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

#[derive(Default)]
pub struct HotRegions {
    pub rects: Mutex<Vec<HotRect>>,
}

#[derive(Serialize, Clone)]
struct CursorEvent {
    x: f64,
    y: f64,
}

pub fn spawn(app: AppHandle, regions: Arc<HotRegions>) {
    thread::Builder::new()
        .name("quill-mouse-arbiter".into())
        .spawn(move || run(app, regions))
        .expect("spawn mouse-arbiter thread");
}

fn run(app: AppHandle, regions: Arc<HotRegions>) {
    let mut last_in_hot = false;
    let mut last_pos = (-1.0_f64, -1.0_f64);
    let mut move_emit_tick = 0u32;
    loop {
        thread::sleep(Duration::from_millis(40));
        let pos = current_mouse_location();
        let in_hot = match regions.rects.lock() {
            Ok(g) => g.iter().any(|r| {
                pos.0 >= r.x && pos.0 <= r.x + r.w &&
                pos.1 >= r.y && pos.1 <= r.y + r.h
            }),
            Err(_) => false,
        };

        if in_hot != last_in_hot {
            if let Some(win) = app.get_webview_window("overlay") {
                let _ = win.set_ignore_cursor_events(!in_hot);
            }
            let evt_name = if in_hot { "cursor-enter-hot" } else { "cursor-leave-hot" };
            eprintln!(
                "[quill][arbiter] {evt_name} pos=({:.0},{:.0}) hot_regions={}",
                pos.0, pos.1,
                regions.rects.lock().map(|g| g.len()).unwrap_or(0)
            );
            let _ = app.emit_to(
                "overlay",
                evt_name,
                CursorEvent { x: pos.0, y: pos.1 },
            );
            last_in_hot = in_hot;
            last_pos = pos;
        } else if in_hot {
            // While inside a hot region, push cursor position updates so JS
            // can switch popovers as user moves between underlines.
            move_emit_tick = move_emit_tick.wrapping_add(1);
            let moved = (pos.0 - last_pos.0).abs() > 2.0 || (pos.1 - last_pos.1).abs() > 2.0;
            if moved || move_emit_tick % 5 == 0 {
                let _ = app.emit_to(
                    "overlay",
                    "cursor-move-hot",
                    CursorEvent { x: pos.0, y: pos.1 },
                );
                last_pos = pos;
            }
        }
    }
}

fn current_mouse_location() -> (f64, f64) {
    let src = match CGEventSource::new(CGEventSourceStateID::HIDSystemState) {
        Ok(s) => s,
        Err(_) => return (-1.0, -1.0),
    };
    let evt = match CGEvent::new(src) {
        Ok(e) => e,
        Err(_) => return (-1.0, -1.0),
    };
    let p = evt.location();
    (p.x, p.y)
}
