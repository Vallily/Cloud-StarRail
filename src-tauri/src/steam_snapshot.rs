//! One-shot snapshot of this application's own client area, held in RAM only.
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
use tauri::Window;
use windows::Win32::{
    Foundation::{HWND, RECT},
    Graphics::Gdi::*,
    Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS},
    UI::WindowsAndMessaging::{GetClientRect, GetWindowThreadProcessId},
};
static BUSY: AtomicBool = AtomicBool::new(false);
struct Busy;
impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Relaxed);
    }
}
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
struct Gdi {
    hwnd: HWND,
    source: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}
impl Drop for Gdi {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.0.is_null() && self.previous.0 as isize != -1 {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.0.is_null() {
                let _ = DeleteObject(self.bitmap.into());
            }
            if !self.memory.0.is_null() {
                let _ = DeleteDC(self.memory);
            }
            if !self.source.0.is_null() {
                ReleaseDC(Some(self.hwnd), self.source);
            }
        }
    }
}
pub fn capture(window: &Window) -> Result<Snapshot, String> {
    let hwnd = window.hwnd().map_err(|e| e.to_string())?.0 as usize;
    if BUSY.swap(true, Ordering::Relaxed) {
        return Err("上一项网页快照尚未完成，请稍后重试。".into());
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    if let Err(error) = std::thread::Builder::new()
        .name("steam-web-snapshot".into())
        .spawn(move || {
            let _busy = Busy;
            let result = unsafe { capture_inner(HWND(hwnd as *mut _)) };
            let _ = tx.send(result);
        })
    {
        BUSY.store(false, Ordering::Relaxed);
        return Err(error.to_string());
    }
    rx.recv_timeout(Duration::from_secs(3))
        .map_err(|_| "网页快照超时，已保留普通网页显示。".to_string())?
}
unsafe fn capture_inner(hwnd: HWND) -> Result<Snapshot, String> {
    let mut pid = 0;
    GetWindowThreadProcessId(hwnd, Some(&mut pid));
    if pid != std::process::id() {
        return Err("只能捕获本程序窗口。".into());
    }
    let mut rect = RECT::default();
    GetClientRect(hwnd, &mut rect).map_err(|e| e.to_string())?;
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    let length = i64::from(width) * i64::from(height) * 4;
    if width <= 0 || height <= 0 || length > 128 * 1024 * 1024 {
        return Err("窗口尺寸不适合本轮快照测试。".into());
    }
    let mut resources = Gdi {
        hwnd,
        source: GetDC(Some(hwnd)),
        memory: HDC::default(),
        bitmap: HBITMAP::default(),
        previous: HGDIOBJ::default(),
    };
    if resources.source.0.is_null() {
        return Err("无法创建窗口快照上下文。".into());
    }
    resources.memory = CreateCompatibleDC(Some(resources.source));
    if resources.memory.0.is_null() {
        return Err("无法创建快照内存上下文。".into());
    }
    let info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    resources.bitmap = CreateDIBSection(
        Some(resources.source),
        &info,
        DIB_RGB_COLORS,
        &mut bits,
        None,
        0,
    )
    .map_err(|e| e.to_string())?;
    if bits.is_null() {
        return Err("快照位图为空。".into());
    }
    resources.previous = SelectObject(resources.memory, resources.bitmap.into());
    if resources.previous.0.is_null() || resources.previous.0 as isize == -1 {
        return Err("无法选择快照位图。".into());
    }
    std::ptr::write_bytes(bits, 0, length as usize);
    // 0x2 is PW_RENDERFULLCONTENT, used for accelerated WebView contents.
    if !PrintWindow(hwnd, resources.memory, PRINT_WINDOW_FLAGS(0x1 | 0x2)).as_bool() {
        return Err("PrintWindow 无法捕获网页。".into());
    }
    let data = std::slice::from_raw_parts(bits as *const u8, length as usize);
    let (mut low, mut high) = (255u8, 0u8);
    for pixel in data.chunks_exact(4).step_by(127) {
        let brightness =
            ((u16::from(pixel[0]) + u16::from(pixel[1]) + u16::from(pixel[2])) / 3) as u8;
        low = low.min(brightness);
        high = high.max(brightness);
    }
    if high < 12 || high.saturating_sub(low) < 6 {
        return Err("网页快照接近黑屏或纯色，本次未切换显示。".into());
    }
    let mut rgba = data.to_vec();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        pixel[3] = 255;
    }
    Ok(Snapshot {
        width: width as u32,
        height: height as u32,
        rgba,
    })
}
