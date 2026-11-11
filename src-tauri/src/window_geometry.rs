//! Native interactive resize constraint. Adjust WM_SIZING before Windows commits
//! the rectangle, avoiding post-resize corrections and visible snapping.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tauri::{Manager, Window};
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM},
    UI::{
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};
fn pack(w: u32, h: u32) -> u64 {
    (u64::from(w) << 32) | u64::from(h)
}
fn unpack(value: u64) -> (i32, i32) {
    ((value >> 32) as i32, value as u32 as i32)
}
struct Constraint {
    ratio: AtomicU64,
    minimum: AtomicU64,
    title_height: AtomicU64,
}
impl Default for Constraint {
    fn default() -> Self {
        Self {
            ratio: AtomicU64::new(0),
            minimum: AtomicU64::new(pack(480, 320)),
            title_height: AtomicU64::new(40),
        }
    }
}
#[derive(Clone, Default)]
pub struct Aspect(Arc<Constraint>);
pub fn initialize(window: &Window, width: u32, height: u32) -> (u32, u32) {
    let state = window.state::<Aspect>();
    let _ = (width, height);
    let target = pack(16, 9);
    let _ = state
        .0
        .ratio
        .compare_exchange(0, target, Ordering::Relaxed, Ordering::Relaxed);
    let (w, h) = unpack(state.0.ratio.load(Ordering::Relaxed));
    update_scale(window);
    (w as u32, h as u32)
}
pub fn update_scale(window: &Window) {
    let scale = window.scale_factor().unwrap_or(1.0);
    let top = if window.is_fullscreen().unwrap_or(false) {
        0
    } else {
        (40.0 * scale).round() as u64
    };
    window
        .state::<Aspect>()
        .0
        .title_height
        .store(top, Ordering::Relaxed);
    window.state::<Aspect>().0.minimum.store(
        pack((480.0 * scale).ceil() as u32, (320.0 * scale).ceil() as u32),
        Ordering::Relaxed,
    );
}
const ID: usize = 0x43534152;
pub fn install(window: &Window) -> tauri::Result<()> {
    let hwnd = window.hwnd()?.0 as usize;
    let constraint = window.state::<Aspect>().0.clone();
    window.run_on_main_thread(move || unsafe {
        let data = Box::into_raw(Box::new(constraint));
        if !SetWindowSubclass(HWND(hwnd as *mut _), Some(sizing), ID, data as usize).as_bool() {
            drop(Box::from_raw(data));
            eprintln!("Could not install window aspect constraint");
        }
    })
}
unsafe extern "system" fn sizing(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        let _ = RemoveWindowSubclass(hwnd, Some(sizing), id);
        drop(Box::from_raw(data as *mut Arc<Constraint>));
        return DefSubclassProc(hwnd, msg, wp, lp);
    }
    if msg == WM_SIZING && lp.0 != 0 {
        let constraint = &*(data as *const Arc<Constraint>);
        let (mut outer, mut client) = (RECT::default(), RECT::default());
        if GetWindowRect(hwnd, &mut outer).is_ok() && GetClientRect(hwnd, &mut client).is_ok() {
            let border = (
                (outer.right - outer.left - client.right + client.left).max(0),
                (outer.bottom - outer.top - client.bottom + client.top).max(0),
            );
            let top = constraint.title_height.load(Ordering::Relaxed) as i32;
            let minimum = unpack(constraint.minimum.load(Ordering::Relaxed));
            constrain(
                &mut *(lp.0 as *mut RECT),
                wp.0 as u32,
                unpack(constraint.ratio.load(Ordering::Relaxed)),
                (minimum.0, (minimum.1 - top).max(1)),
                (border.0, border.1 + top),
            );
            return LRESULT(1);
        }
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}
fn rounded(value: i64, numerator: i64, denominator: i64) -> i32 {
    ((value * numerator + denominator / 2) / denominator) as i32
}
fn constrain(
    rect: &mut RECT,
    edge: u32,
    ratio: (i32, i32),
    minimum: (i32, i32),
    border: (i32, i32),
) {
    if !(1..=8).contains(&edge) || ratio.0 <= 0 || ratio.1 <= 0 {
        return;
    }
    let w = (rect.right - rect.left - border.0).max(1);
    let h = (rect.bottom - rect.top - border.1).max(1);
    let rw = i64::from(ratio.0);
    let rh = i64::from(ratio.1);
    let (mut w, mut h) = match edge {
        WMSZ_LEFT | WMSZ_RIGHT => (w, rounded(i64::from(w), rh, rw)),
        WMSZ_TOP | WMSZ_BOTTOM => (rounded(i64::from(h), rw, rh), h),
        _ => {
            // Project the requested corner onto the aspect-ratio line. This
            // uses both coordinates and has no dependency on the previously
            // constrained size, so width/height cannot alternate as drivers.
            let denominator = rw * rw + rh * rh;
            let numerator = (i64::from(w) * rw + i64::from(h) * rh) * rw;
            let width = ((numerator + denominator / 2) / denominator).max(1) as i32;
            (width, rounded(i64::from(width), rh, rw))
        }
    };
    if w < minimum.0 {
        w = minimum.0;
        h = rounded(i64::from(w), rh, rw);
    }
    if h < minimum.1 {
        h = minimum.1;
        w = rounded(i64::from(h), rw, rh);
    }
    w += border.0;
    h += border.1;
    if matches!(edge, WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT) {
        rect.left = rect.right - w;
    } else {
        rect.right = rect.left + w;
    }
    if matches!(edge, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT) {
        rect.top = rect.bottom - h;
    } else {
        rect.bottom = rect.top + h;
    }
}
pub fn title_pixels(window: &Window) -> u32 {
    window
        .state::<Aspect>()
        .0
        .title_height
        .load(Ordering::Relaxed) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_edges_keep_viewport_ratio_and_opposite_anchors() {
        for edge in 1..=8 {
            let mut r = RECT {
                left: -270,
                top: 75,
                right: 1250,
                bottom: 990,
            };
            let before = r;
            // At 125% DPI the 40 logical-pixel title is 50 physical pixels.
            constrain(&mut r, edge, (16, 9), (480, 270), (16, 19 + 50));
            let w = r.right - r.left - 16;
            let h = r.bottom - r.top - 19 - 50;
            assert!(
                (f64::from(w) / f64::from(h) - 16.0 / 9.0).abs() < 0.003,
                "edge {edge}"
            );
            if matches!(edge, WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT) {
                assert_eq!(r.right, before.right);
            } else {
                assert_eq!(r.left, before.left);
            }
            if matches!(edge, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT) {
                assert_eq!(r.bottom, before.bottom);
            } else {
                assert_eq!(r.top, before.top);
            }
        }
    }
    #[test]
    fn outward_corner_drag_never_shrinks_and_corrections_are_stable() {
        for edge in [
            WMSZ_TOPLEFT,
            WMSZ_TOPRIGHT,
            WMSZ_BOTTOMLEFT,
            WMSZ_BOTTOMRIGHT,
        ] {
            let mut previous = (1280, 720);
            // The old incremental dominant-axis heuristic grew to width 1289
            // on the first point, then shrank to 1282 on the second point.
            for (dx, dy) in [(1, 5), (2, 6), (3, 7), (4, 8), (5, 9), (6, 10)] {
                let (width, height) = (1280 + dx, 720 + dy);
                let leftward = matches!(edge, WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT);
                let upward = matches!(edge, WMSZ_TOPLEFT | WMSZ_TOPRIGHT);
                let mut r = RECT {
                    left: if leftward { -width } else { 0 },
                    right: if leftward { 0 } else { width },
                    top: if upward { -height } else { 0 },
                    bottom: if upward { 0 } else { height },
                };
                constrain(&mut r, edge, (16, 9), (480, 270), (0, 0));
                let actual = (r.right - r.left, r.bottom - r.top);
                assert!(
                    actual.0 >= previous.0 && actual.1 >= previous.1,
                    "edge {edge}: {previous:?} -> {actual:?}"
                );
                let settled = r;
                constrain(&mut r, edge, (16, 9), (480, 270), (0, 0));
                assert_eq!(r, settled, "repeated correction must not move the corner");
                previous = actual;
            }
        }
    }
    #[test]
    fn minimum_and_portrait_ratio_survive_small_rectangles() {
        for ratio in [(16, 9), (21, 9), (9, 16)] {
            for edge in 1..=8 {
                let mut r = RECT {
                    left: 50,
                    top: 100,
                    right: 100,
                    bottom: 140,
                };
                constrain(&mut r, edge, ratio, (600, 400), (12, 12));
                let w = r.right - r.left - 12;
                let h = r.bottom - r.top - 12;
                assert!(w >= 600 && h >= 400);
                assert!(
                    (f64::from(w) / f64::from(h) - f64::from(ratio.0) / f64::from(ratio.1)).abs()
                        < 0.005
                );
            }
        }
    }
}
