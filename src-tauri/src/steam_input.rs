//! Input-only diagnostic. Never synthesizes Steam keys or initializes Steamworks.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::Window;
use windows::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    UI::{
        Input::KeyboardAndMouse::{GetAsyncKeyState, GetFocus, SetFocus},
        Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

static NATIVE_MODE: AtomicBool = AtomicBool::new(false);
static TAB_NATIVE: AtomicU64 = AtomicU64::new(0);
static SHIFT_NATIVE: AtomicU64 = AtomicU64::new(0);
static F12_NATIVE: AtomicU64 = AtomicU64::new(0);
static TAB_WEB: AtomicU64 = AtomicU64::new(0);
static SHIFT_WEB: AtomicU64 = AtomicU64::new(0);
static F12_WEB: AtomicU64 = AtomicU64::new(0);
const SUBCLASS_ID: usize = 0x4353494e;

fn count(key: usize, native: bool) {
    let counter = match (key, native) {
        (9, true) => &TAB_NATIVE,
        (16, true) => &SHIFT_NATIVE,
        (123, true) => &F12_NATIVE,
        (9, false) => &TAB_WEB,
        (16, false) => &SHIFT_WEB,
        (123, false) => &F12_WEB,
        _ => return,
    };
    counter.fetch_add(1, Ordering::Relaxed);
}
pub fn observe_web_key(key: u32, down: bool) {
    if down
        && std::env::args_os()
            .any(|arg| arg == "--steam-input-test" || arg == "--steam-web-overlay")
    {
        count(key as usize, false);
    }
}
unsafe extern "system" fn input_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    if msg == WM_NCDESTROY {
        let _ = RemoveWindowSubclass(hwnd, Some(input_proc), id);
    }
    if NATIVE_MODE.load(Ordering::Relaxed) {
        // Wry's parent subclass redirects WM_SETFOCUS into its WebView.
        // Only bypass that redirection while all views are deliberately hidden.
        if msg == WM_SETFOCUS {
            return LRESULT(0);
        }
        if msg == WM_LBUTTONDOWN {
            let _ = SetFocus(Some(hwnd));
        }
    }
    if msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN {
        count(wp.0, true);
    }
    DefSubclassProc(hwnd, msg, wp, lp)
}

pub fn install(window: &Window) -> Result<(), String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as usize;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || unsafe {
            let ok =
                SetWindowSubclass(HWND(hwnd as *mut _), Some(input_proc), SUBCLASS_ID, 0).as_bool();
            let _ = tx.send(ok);
        })
        .map_err(|e| e.to_string())?;
    if rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .map_err(|e| e.to_string())?
    {
        Ok(())
    } else {
        Err("Cannot install input diagnostic subclass".into())
    }
}
pub fn native_focus(window: &Window) -> Result<String, String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as usize;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    window
        .run_on_main_thread(move || unsafe {
            let target = HWND(hwnd as *mut _);
            if GetForegroundWindow() != target {
                let _ = tx.send(Err(
                    "Window is not foreground; focus was not forced".to_string()
                ));
                return;
            }
            NATIVE_MODE.store(true, Ordering::Relaxed);
            let before = GetFocus();
            let _ = SetFocus(Some(target));
            let after = GetFocus();
            let text = format!(
                "focus before={before:?} after={after:?} target={target:?} verified={}",
                after == target
            );
            let _ = tx.send(if after == target { Ok(text) } else { Err(text) });
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(std::time::Duration::from_secs(3))
        .map_err(|e| e.to_string())?
}
pub fn release_focus() {
    NATIVE_MODE.store(false, Ordering::Relaxed);
}
pub unsafe fn toggle_down(web_overlay: bool) -> bool {
    if web_overlay {
        GetAsyncKeyState(0x11) < 0 && GetAsyncKeyState(0x10) < 0 && GetAsyncKeyState(0x4f) < 0
    } else {
        GetAsyncKeyState(0x77) < 0
    }
}
pub unsafe fn ours_foreground(window: &Window) -> bool {
    window
        .hwnd()
        .is_ok_and(|hwnd| GetForegroundWindow() == HWND(hwnd.0))
}
pub unsafe fn status(window: &Window) -> String {
    let Ok(hwnd) = window.hwnd() else {
        return "window unavailable".into();
    };
    let hwnd = HWND(hwnd.0);
    let tid = GetWindowThreadProcessId(hwnd, None);
    let mut info = GUITHREADINFO {
        cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
        ..Default::default()
    };
    let read = GetGUIThreadInfo(tid, &mut info);
    let mut class = [0u16; 128];
    let length = GetClassNameW(info.hwndFocus, &mut class).max(0) as usize;
    format!("native_mode={} foreground_main={} focus_main={} focus={:?} class={} query_ok={} native[tab,shift,f12]={},{},{} web[tab,shift,f12]={},{},{}",
        NATIVE_MODE.load(Ordering::Relaxed), GetForegroundWindow() == hwnd, info.hwndFocus == hwnd,
        info.hwndFocus, String::from_utf16_lossy(&class[..length]), read.is_ok(),
        TAB_NATIVE.load(Ordering::Relaxed), SHIFT_NATIVE.load(Ordering::Relaxed), F12_NATIVE.load(Ordering::Relaxed),
        TAB_WEB.load(Ordering::Relaxed), SHIFT_WEB.load(Ordering::Relaxed), F12_WEB.load(Ordering::Relaxed))
}

/// Consume only the dedicated test toggle, leaving Steam shortcuts untouched.
pub fn is_web_toggle(key: u32) -> bool {
    key == 0x4f
        && std::env::args_os().any(|arg| arg == "--steam-web-overlay")
        && unsafe { GetAsyncKeyState(0x11) < 0 && GetAsyncKeyState(0x10) < 0 }
}
