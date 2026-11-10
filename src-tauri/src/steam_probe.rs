//! Shared D3D11 presentation for the native diagnostic window and direct main-
//! window experiment. No Steamworks AppID, key forwarding or DLL injection.
use std::{
    fs::OpenOptions,
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Manager, Window};
use windows::{
    core::{w, Interface, Result},
    Win32::{
        Foundation::{HMODULE, RECT},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK},
    },
};

static WEBVIEWS_HIDDEN: AtomicBool = AtomicBool::new(false);

pub fn webviews_hidden() -> bool {
    WEBVIEWS_HIDDEN.load(Ordering::Relaxed)
}

// Restore on success and error, including partial hide failures.
struct VisibilityRestore {
    window: Window,
    views: Vec<tauri::Webview>,
}
impl VisibilityRestore {
    fn hide(window: &Window) -> tauri::Result<Self> {
        let mut restore = Self {
            window: window.clone(),
            views: Vec::new(),
        };
        WEBVIEWS_HIDDEN.store(true, Ordering::Relaxed);
        for label in ["main", "game", "controls", "exit-confirm"] {
            if let Some(view) = window.app_handle().get_webview(label) {
                restore.views.push(view.clone());
                view.hide()?;
            }
        }
        Ok(restore)
    }
}
impl Drop for VisibilityRestore {
    fn drop(&mut self) {
        let restore_focus = unsafe { crate::steam_input::ours_foreground(&self.window) };
        crate::steam_input::release_focus();
        WEBVIEWS_HIDDEN.store(false, Ordering::Relaxed);
        for view in &self.views {
            // Shell/game are always visible in the normal client. Layout
            // restores controls and confirmation from their application state.
            let _ = if matches!(view.label(), "main" | "game") {
                view.show()
            } else {
                view.hide()
            };
        }
        let _ = crate::layout(&self.window);
        if restore_focus {
            let _ = crate::focus_game(&self.window);
        }
    }
}

struct ProbeLog(std::path::PathBuf);
impl ProbeLog {
    fn new() -> Self {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("io.github.starrail-cloud-desktop")
            .join("SteamProbe");
        let _ = std::fs::create_dir_all(&base);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self(base.join(format!("probe-{stamp}-{}.log", std::process::id())))
    }
    fn line(&self, message: impl std::fmt::Display) {
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.0) {
            let _ = writeln!(file, "{message}");
        }
    }
}

/// Attach the same native presentation path to the existing game HWND.
/// No additional window, capture session or synthetic input is created.
pub fn start_on_main(window: Window) {
    let _ = std::thread::Builder::new()
        .name("steam-direct-d3d11".into())
        .spawn(move || {
            let log = ProbeLog::new();
            log.line(format!(
                "Cloud-StarRail {} | direct main HWND | pid={}",
                env!("CARGO_PKG_VERSION"),
                std::process::id()
            ));
            // Main-window lifetime is checked by the render loop. Closing the
            // confirmation dialog does not stop presentation prematurely.
            let stop = AtomicBool::new(false);
            if let Err(error) = unsafe { render(&window, &stop, &log, false) } {
                log.line(format!("ERROR: {error}"));
                // Drop the D3D objects and leave the existing WebViews running.
                if window.app_handle().get_window("main").is_some() {
                    let message = windows::core::HSTRING::from(format!(
                        "Steam 直接接入测试已停止：{error}\n网页窗口继续运行。\n日志：{}",
                        log.0.display()
                    ));
                    unsafe {
                        let _ = MessageBoxW(
                            None,
                            &message,
                            w!("Steam 直接接入测试"),
                            MB_OK | MB_ICONERROR,
                        );
                    }
                }
            }
        });
}

pub fn run() {
    let log = Arc::new(ProbeLog::new());
    log.line(format!(
        "Cloud-StarRail {} | D3D11 native probe | pid={}",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    ));
    for key in ["SteamAppId", "SteamGameId"] {
        log.line(format!(
            "{key}={}",
            std::env::var(key).unwrap_or_else(|_| "not set".into())
        ));
    }
    let stop = Arc::new(AtomicBool::new(false));
    let stop_events = stop.clone();
    tauri::Builder::default()
        .setup(move |app| {
            let window = tauri::window::WindowBuilder::new(app, "steam-probe")
                .title("Steam 原生渲染验证 | 请使用 Steam 配置的叠加与截图快捷键")
                .inner_size(1100., 680.)
                .min_inner_size(480., 320.)
                .center()
                .build()?;
            let window_copy = window.clone();
            let log_copy = log.clone();
            let stop_copy = stop.clone();
            std::thread::Builder::new()
                .name("steam-probe-d3d11".into())
                .spawn(move || {
                    // SAFETY: all D3D interfaces are created and used on this render
                    // thread. The Window keeps its HWND alive until shutdown is signaled.
                    if let Err(error) = unsafe { render(&window_copy, &stop_copy, &log_copy, true) }
                    {
                        log_copy.line(format!("ERROR: {error}"));
                        let message = windows::core::HSTRING::from(format!(
                            "D3D11 验证无法运行：{error}\n日志：{}",
                            log_copy.0.display()
                        ));
                        unsafe {
                            let _ = MessageBoxW(
                                None,
                                &message,
                                w!("Steam 验证失败"),
                                MB_OK | MB_ICONERROR,
                            );
                        }
                        window_copy.app_handle().exit(1);
                    }
                })?;
            Ok(())
        })
        .on_window_event(move |window, event| {
            if matches!(
                event,
                tauri::WindowEvent::CloseRequested { .. } | tauri::WindowEvent::Destroyed
            ) {
                stop_events.store(true, Ordering::Relaxed);
                if matches!(event, tauri::WindowEvent::Destroyed) {
                    window.app_handle().exit(0);
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("Could not start Steam native probe");
}

unsafe fn target(device: &ID3D11Device, swap: &IDXGISwapChain) -> Result<ID3D11RenderTargetView> {
    let buffer: ID3D11Texture2D = swap.GetBuffer(0)?;
    let mut view = None;
    device.CreateRenderTargetView(&buffer, None, Some(&mut view))?;
    view.ok_or_else(windows::core::Error::from_thread)
}

unsafe fn render(
    window: &Window,
    stop: &AtomicBool,
    log: &ProbeLog,
    diagnostic: bool,
) -> Result<()> {
    let size = window
        .inner_size()
        .map_err(|_| windows::core::Error::from_thread())?;
    let hwnd = window
        .hwnd()
        .map_err(|_| windows::core::Error::from_thread())?;
    let desc = DXGI_SWAP_CHAIN_DESC {
        BufferDesc: DXGI_MODE_DESC {
            Width: size.width.max(1),
            Height: size.height.max(1),
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            ..Default::default()
        },
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: 2,
        OutputWindow: windows::Win32::Foundation::HWND(hwnd.0),
        Windowed: true.into(),
        SwapEffect: DXGI_SWAP_EFFECT_DISCARD,
        ..Default::default()
    };
    let (mut swap, mut device, mut context) = (None, None, None);
    D3D11CreateDeviceAndSwapChain(
        None,
        D3D_DRIVER_TYPE_HARDWARE,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_FLAG(0),
        Some(&[D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_10_1]),
        D3D11_SDK_VERSION,
        Some(&desc),
        Some(&mut swap),
        Some(&mut device),
        None,
        Some(&mut context),
    )?;
    let swap = swap.ok_or_else(windows::core::Error::from_thread)?;
    let device = device.ok_or_else(windows::core::Error::from_thread)?;
    let context = context.ok_or_else(windows::core::Error::from_thread)?;
    let context1: ID3D11DeviceContext1 = context.cast()?;
    let mut view = Some(target(&device, &swap)?);
    let mut dimensions = (size.width, size.height);
    let started = Instant::now();
    let mut status_at = Instant::now() - Duration::from_secs(2);
    let mut frames: u64 = 0;
    let isolation = !diagnostic && std::env::args_os().any(|arg| arg == "--steam-isolate");
    let web_overlay = !diagnostic && std::env::args_os().any(|arg| arg == "--steam-web-overlay");
    let input_test =
        web_overlay || (!diagnostic && std::env::args_os().any(|arg| arg == "--steam-input-test"));
    let mut snapshot_texture: Option<ID3D11Texture2D> = None;
    // Development-only one-shot capture check; never enabled in a release build.
    let smoke = web_overlay
        && cfg!(debug_assertions)
        && std::env::var_os("CLOUD_STARRAIL_SNAPSHOT_SMOKE").is_some();
    let mut smoke_done = false;
    let mut last_f8 = false;
    let mut input_started = None;
    if input_test {
        crate::steam_input::install(window).map_err(|error| {
            log.line(format!("INPUT install failed: {error}"));
            windows::core::Error::from_thread()
        })?;
        log.line(if web_overlay {
            "WEB: Ctrl+Shift+O captures webpage backdrop and transfers native focus; press Steam hotkey separately. Toggle again to restore; timeout 90s."
        } else { "INPUT: F8 toggles WebView/native focus; auto restore after 90s. No Steam keys synthesized." });
    }
    let mut isolation_stage = 0;
    let mut visibility_restore = None;
    if isolation {
        log.line("ISOLATION: 0-15s WebViews; 15-35s hidden without explicit focus; 35-65s hidden with main focus; 65s restore.");
    }
    log.line("D3D11 hardware device and swapchain created; native Present is active.");
    log.line(if diagnostic {
        "No shortcut key is intercepted. Close the native window or use Alt+F4 to exit this test."
    } else if web_overlay {
        "Single main HWND with one-shot webpage backdrop; no extra window or Steam key synthesis. Host Alt+F4/F11 retained."
    } else {
        "Existing main HWND and WebView input preserved; no capture, extra window or key forwarding. Host Alt+F4/F11 retained."
    });
    while !stop.load(Ordering::Relaxed) {
        if window.app_handle().get_window(window.label()).is_none() {
            break;
        }
        if input_test {
            let f8 = crate::steam_input::toggle_down(web_overlay);
            let smoke_toggle = smoke && !smoke_done && started.elapsed() >= Duration::from_secs(3);
            smoke_done |= smoke_toggle;
            let toggle =
                (f8 && !last_f8 && crate::steam_input::ours_foreground(window)) || smoke_toggle;
            last_f8 = f8;
            let expired = input_started
                .is_some_and(|time: Instant| time.elapsed() >= Duration::from_secs(90));
            if toggle || expired {
                if visibility_restore.is_some() {
                    drop(visibility_restore.take());
                    snapshot_texture = None;
                    input_started = None;
                    log.line("INPUT: WebViews restored, native-focus override released.");
                } else if toggle {
                    if web_overlay {
                        match crate::steam_snapshot::capture(window) {
                            Ok(snapshot) => {
                                if (snapshot.width, snapshot.height) != dimensions {
                                    log.line("WEB: snapshot size changed; keep webpage visible and retry when stable.");
                                    continue;
                                }
                                let desc = D3D11_TEXTURE2D_DESC {
                                    Width: snapshot.width,
                                    Height: snapshot.height,
                                    MipLevels: 1,
                                    ArraySize: 1,
                                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                                    SampleDesc: DXGI_SAMPLE_DESC {
                                        Count: 1,
                                        Quality: 0,
                                    },
                                    Usage: D3D11_USAGE_DEFAULT,
                                    ..Default::default()
                                };
                                let data = D3D11_SUBRESOURCE_DATA {
                                    pSysMem: snapshot.rgba.as_ptr() as *const _,
                                    SysMemPitch: snapshot.width * 4,
                                    SysMemSlicePitch: 0,
                                };
                                device.CreateTexture2D(
                                    &desc,
                                    Some(&data),
                                    Some(&mut snapshot_texture),
                                )?;
                                log.line(format!(
                                    "WEB: captured {}x{} backdrop in RAM; uploaded native texture.",
                                    snapshot.width, snapshot.height
                                ));
                            }
                            Err(error) => {
                                log.line(format!("WEB: snapshot failed: {error}"));
                                let text = windows::core::HSTRING::from(format!(
                                    "{error}\n网页仍保持正常显示。"
                                ));
                                let _ = MessageBoxW(
                                    Some(windows::Win32::Foundation::HWND(hwnd.0)),
                                    &text,
                                    w!("网页叠加测试"),
                                    MB_OK | MB_ICONERROR,
                                );
                                continue;
                            }
                        }
                    }
                    visibility_restore =
                        Some(VisibilityRestore::hide(window).map_err(|error| {
                            log.line(format!("INPUT hide failed: {error}"));
                            windows::core::Error::from_thread()
                        })?);
                    let focus = crate::steam_input::native_focus(window);
                    log.line(format!("INPUT: native focus request: {focus:?}"));
                    if focus.is_err() {
                        drop(visibility_restore.take());
                        snapshot_texture = None;
                        log.line("INPUT: focus verification failed; restored WebViews.");
                    } else {
                        input_started = Some(Instant::now());
                    }
                }
            }
        }
        if window.is_minimized().unwrap_or(true) {
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }
        let Ok(size) = window.inner_size() else {
            break;
        };
        if size.width == 0 || size.height == 0 {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if dimensions != (size.width, size.height) {
            if web_overlay && visibility_restore.is_some() {
                drop(visibility_restore.take());
                snapshot_texture = None;
                input_started = None;
                log.line(
                    "WEB: resize detected; restored live webpage instead of stretching backdrop.",
                );
            }
            view.take();
            context.ClearState();
            swap.ResizeBuffers(
                2,
                size.width,
                size.height,
                DXGI_FORMAT_UNKNOWN,
                DXGI_SWAP_CHAIN_FLAG(0),
            )?;
            view = Some(target(&device, &swap)?);
            dimensions = (size.width, size.height);
            log.line(format!("Resize: {}x{}", size.width, size.height));
        }
        let view = view
            .as_ref()
            .ok_or_else(windows::core::Error::from_thread)?;
        context.ClearRenderTargetView(view, &[0.035, 0.06, 0.11, 1.]);
        let elapsed = started.elapsed().as_secs_f64();
        if isolation {
            let stage = if elapsed < 15. {
                0
            } else if elapsed < 35. {
                1
            } else if elapsed < 65. {
                2
            } else {
                3
            };
            if stage != isolation_stage {
                if (stage == 1 || stage == 2) && visibility_restore.is_none() {
                    visibility_restore =
                        Some(VisibilityRestore::hide(window).map_err(|error| {
                            log.line(format!("ISOLATION hide error: {error}"));
                            windows::core::Error::from_thread()
                        })?);
                    log.line("ISOLATION stage 1: WebViews hidden; no explicit focus request; ORANGE strip.");
                }
                if stage == 2 {
                    let result = window.set_focus();
                    log.line(format!(
                        "ISOLATION stage 2: main focus requested: {result:?}; GREEN strip."
                    ));
                } else if stage == 3 {
                    drop(visibility_restore.take());
                    log.line("ISOLATION stage 3: WebViews restored; diagnosis complete.");
                }
                isolation_stage = stage;
            }
        }
        if diagnostic
            || (isolation && (1..=2).contains(&isolation_stage))
            || (input_test && !web_overlay && visibility_restore.is_some())
        {
            let width = size.width as i32;
            let height = size.height as i32;

            let bar_width = (width / 7).max(1);
            let x = ((elapsed * 190.) as i32) % (width + bar_width) - bar_width;
            let moving = RECT {
                left: x.max(0),
                top: height / 3,
                right: (x + bar_width).clamp(0, width),
                bottom: height * 2 / 3,
            };
            context1.ClearView(view, &[0.18, 0.65, 0.88, 1.], Some(&[moving]));
            // Reference strips and a binary frame counter appear in Steam captures.
            let top = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: (height / 12).max(1),
            };
            let strip = if input_test {
                [0.1, 0.75, 0.35, 1.]
            } else if isolation && isolation_stage == 1 {
                [0.95, 0.5, 0.1, 1.]
            } else if isolation && isolation_stage == 2 {
                [0.1, 0.75, 0.35, 1.]
            } else {
                [0.65, 0.25, 0.3, 1.]
            };
            context1.ClearView(view, &strip, Some(&[top]));
            for bit in 0..16 {
                let cell = width / 18;
                let rect = RECT {
                    left: (bit + 1) * cell,
                    top: height * 5 / 6,
                    right: (bit + 1) * cell + cell * 3 / 4,
                    bottom: height * 11 / 12,
                };
                let color = if (frames >> bit) & 1 == 1 {
                    [0.85, 0.75, 0.45, 1.]
                } else {
                    [0.12, 0.17, 0.25, 1.]
                };
                context1.ClearView(view, &color, Some(&[rect]));
            }
        }
        if let Some(texture) = &snapshot_texture {
            let back: ID3D11Texture2D = swap.GetBuffer(0)?;
            context.CopyResource(&back, texture);
        }
        let result = swap.Present(1, DXGI_PRESENT(0));
        if stop.load(Ordering::Relaxed) {
            break;
        }
        result.ok()?;
        frames += 1;
        if status_at.elapsed() >= Duration::from_secs(1) {
            let injected = GetModuleHandleW(w!("gameoverlayrenderer64.dll")).is_ok();
            if diagnostic {
                let _ = window.set_title(&format!(
                    "Steam 原生渲染验证 | D3D11 | Steam 模块：{} | 帧数：{frames}",
                    if injected {
                        "已加载"
                    } else {
                        "未检测到"
                    }
                ));
            }
            log.line(format!(
                "frames={frames} elapsed={elapsed:.1}s steam_overlay_module={injected}"
            ));
            if input_test {
                log.line(format!("INPUT: {}", crate::steam_input::status(window)));
            }
            status_at = Instant::now();
        }
    }
    log.line(format!("Probe stopped after {frames} frames."));
    Ok(())
}
