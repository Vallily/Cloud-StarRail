#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tauri::{
    webview::NewWindowResponse, LogicalPosition, LogicalSize, Manager, Webview, WebviewBuilder,
    WebviewUrl, WebviewWindowBuilder, Window,
};
const TITLE: &str = "云·星穹铁道";
const URL: &str = "https://sr.mihoyo.com/cloud/#/";
struct ControlsOpen(AtomicBool);
struct ExitApproved(AtomicBool);
struct ConfirmationOpen(AtomicBool);
static POPUP: AtomicUsize = AtomicUsize::new(0);

fn main() {
    tauri::Builder::default()
        .manage(ControlsOpen(AtomicBool::new(false)))
        .manage(ExitApproved(AtomicBool::new(false)))
        .manage(ConfirmationOpen(AtomicBool::new(false)))
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(w) = app.get_window("main") {
                report(w.unminimize());
                report(w.show());
                report(w.set_focus());
            }
        }))
        .invoke_handler(tauri::generate_handler![window_action])
        .setup(|app| {
            let profile = app.path().app_local_data_dir()?.join("WebView2");
            std::fs::create_dir_all(&profile)?;
            let main = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title(TITLE)
                .decorations(false)
                .inner_size(1280., 760.)
                .min_inner_size(480., 320.)
                .visible(false)
                .build()?;
            // Use the display containing the cursor at launch, then the window's display on restore.
            let cursor = app.cursor_position()?;
            let monitor = app
                .monitor_from_point(cursor.x, cursor.y)?
                .or(app.primary_monitor()?);
            let native = app.get_window("main").expect("main window exists");
            fit_window(&native, monitor)?;
            let popup_app = app.handle().clone();
            let popup_profile = profile.clone();
            let game = native.add_child(
                WebviewBuilder::new("game", WebviewUrl::External(URL.parse()?))
                    .data_directory(profile)
                    .disable_drag_drop_handler()
                    .on_navigation(|url| matches!(url.scheme(), "https" | "http"))
                    .on_new_window(move |url, features| {
                        if !matches!(url.scheme(), "https" | "http")
                            && url.as_str() != "about:blank"
                        {
                            return NewWindowResponse::Deny;
                        }
                        let label = format!("popup-{}", POPUP.fetch_add(1, Ordering::Relaxed));
                        match WebviewWindowBuilder::new(
                            &popup_app,
                            label,
                            WebviewUrl::External("about:blank".parse().unwrap()),
                        )
                        .window_features(features)
                        .data_directory(popup_profile.clone())
                        .title(TITLE)
                        .on_navigation(|u| {
                            matches!(u.scheme(), "https" | "http") || u.as_str() == "about:blank"
                        })
                        .build()
                        {
                            Ok(window) => NewWindowResponse::Create { window },
                            Err(e) => {
                                eprintln!("Popup failed: {e}");
                                NewWindowResponse::Deny
                            }
                        }
                    }),
                LogicalPosition::new(0., 40.),
                LogicalSize::new(1280., 720.),
            )?;
            native.add_child(
                WebviewBuilder::new("controls", WebviewUrl::App("controls.html".into()))
                    .transparent(true)
                    .background_color(tauri::webview::Color(0, 0, 0, 0)),
                LogicalPosition::new(1104., 0.),
                LogicalSize::new(176., 40.),
            )?;
            // A child view is clipped by the actual main-window boundary, including
            // DWM corners. Showing it does not run a native window opening animation.
            let confirmation = native.add_child(
                WebviewBuilder::new("exit-confirm", WebviewUrl::App("confirm.html".into()))
                    .transparent(true)
                    .background_color(tauri::webview::Color(0, 0, 0, 0)),
                LogicalPosition::new(0., 0.),
                LogicalSize::new(1280., 760.),
            )?;
            confirmation.hide()?;
            install_keys(&game)?;
            if let Some(chrome) = app.get_webview("controls") {
                install_keys(&chrome)?;
            }
            if let Some(shell) = app.get_webview("main") {
                install_keys(&shell)?;
            }
            layout(&native)?;
            main.show()?;
            start_edge_watcher(app.handle().clone());
            Ok(())
        })
        .on_window_event(|w, event| {
            if w.label() != "main" {
                return;
            }
            match event {
                tauri::WindowEvent::Resized(_) | tauri::WindowEvent::ScaleFactorChanged { .. } => {
                    report(layout(w))
                }
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    if !w.state::<ExitApproved>().0.load(Ordering::Relaxed) {
                        api.prevent_close();
                        report(show_exit_confirmation(w));
                    }
                }
                tauri::WindowEvent::Destroyed => w.app_handle().exit(0),
                _ => {}
            }
        })
        .run(tauri::generate_context!())
        .expect("Could not start Cloud Star Rail");
}

// Commands are limited to the two bundled UI views; the remote game view has no native access.
#[tauri::command]
async fn window_action(webview: Webview, action: String) -> Result<(), String> {
    if !matches!(webview.label(), "main" | "controls" | "exit-confirm") {
        return Err("Untrusted view".into());
    }
    let url = webview.url().map_err(|e| e.to_string())?;
    if url.scheme() != "tauri"
        && url.host_str() != Some("tauri.localhost")
        && url.host_str() != Some("localhost")
        && url.host_str() != Some("127.0.0.1")
    {
        return Err("Untrusted origin".into());
    }
    if webview.label() == "exit-confirm" {
        return match action.as_str() {
            "cancel_exit" => {
                let main = webview.window();
                main.state::<ConfirmationOpen>()
                    .0
                    .store(false, Ordering::Relaxed);
                webview
                    .hide()
                    .and_then(|()| focus_game(&main))
                    .map_err(|e| e.to_string())
            }
            "dialog_ready" => sync_confirmation(&webview.window()).map_err(|e| e.to_string()),
            "confirm_exit" => {
                let main = webview
                    .app_handle()
                    .get_window("main")
                    .ok_or("Missing main window")?;
                main.state::<ExitApproved>()
                    .0
                    .store(true, Ordering::Relaxed);
                main.close().map_err(|e| e.to_string())
            }
            _ => Err("Unknown dialog action".into()),
        };
    }
    let w = webview.window();
    if w.state::<ConfirmationOpen>().0.load(Ordering::Relaxed) && action != "sync" {
        return Ok(());
    }
    let result = match action.as_str() {
        "minimize" => w.minimize(),
        "toggle" => toggle(&w),
        "close" => w.close(),
        "drag" => w.start_dragging(),
        "sync" => layout(&w),
        "controls_open" | "controls_close" => {
            if w.is_fullscreen().map_err(|e| e.to_string())? {
                w.state::<ControlsOpen>()
                    .0
                    .store(action == "controls_open", Ordering::Relaxed);
            }
            sync_controls(&w)
        }
        _ => return Err("Unknown window action".into()),
    };
    result.map_err(|e| e.to_string())
}

fn fit_window(w: &Window, monitor: Option<tauri::Monitor>) -> tauri::Result<()> {
    if let Some(m) = monitor {
        let screen = m.size();
        let area = m.work_area();
        // Whole window, including the 40px title area, follows the monitor's aspect ratio.
        let factor = (f64::from(area.size.width) / f64::from(screen.width))
            .min(f64::from(area.size.height) / f64::from(screen.height))
            * 0.82;
        let width = (f64::from(screen.width) * factor).round() as u32;
        let height = (f64::from(screen.height) * factor).round() as u32;
        w.set_size(tauri::PhysicalSize::new(width, height))?;
        w.set_position(tauri::PhysicalPosition::new(
            area.position.x + (area.size.width as i32 - width as i32) / 2,
            area.position.y + (area.size.height as i32 - height as i32) / 2,
        ))?;
    }
    Ok(())
}

fn toggle(w: &Window) -> tauri::Result<()> {
    if w.state::<ConfirmationOpen>().0.load(Ordering::Relaxed) {
        return Ok(());
    }
    let fullscreen = w.is_fullscreen()?;
    let monitor = w.current_monitor()?;
    w.state::<ControlsOpen>().0.store(false, Ordering::Relaxed);
    w.set_fullscreen(!fullscreen)?;
    if fullscreen {
        fit_window(w, monitor)?;
    }
    layout(w)?;
    focus_game(w)
}

fn layout(w: &Window) -> tauri::Result<()> {
    let size = w.inner_size()?.to_logical::<f64>(w.scale_factor()?);
    if size.width < 1. || size.height < 1. {
        return Ok(());
    }
    let fullscreen = w.is_fullscreen()?;
    let top = if fullscreen { 0. } else { 40. };
    if let Some(game) = w.app_handle().get_webview("game") {
        game.set_position(LogicalPosition::new(0., top))?;
        game.set_size(LogicalSize::new(size.width, (size.height - top).max(1.)))?;
    }
    sync_controls(w)?;
    sync_confirmation(w)
}

fn sync_controls(w: &Window) -> tauri::Result<()> {
    let size = w.inner_size()?.to_logical::<f64>(w.scale_factor()?);
    let fullscreen = w.is_fullscreen()?;
    if let Some(chrome) = w.app_handle().get_webview("controls") {
        let open = w.state::<ControlsOpen>().0.load(Ordering::Relaxed);
        // Only mode/viewport changes need geometry updates; hover does not resize the game.
        let expected = LogicalSize::new(176., if fullscreen { 64. } else { 40. });
        let actual = chrome.size()?.to_logical::<f64>(w.scale_factor()?);
        if actual != expected {
            chrome.set_size(expected)?;
        }
        let position = LogicalPosition::new((size.width - 176.).max(0.), 0.);
        if chrome.position()?.to_logical::<f64>(w.scale_factor()?) != position {
            chrome.set_position(position)?;
        }
        chrome.eval(format!("window.setFullscreen?.({fullscreen}, {open})"))?;
        if fullscreen && !open {
            chrome.hide()?;
        } else {
            chrome.show()?;
        }
    }
    Ok(())
}
#[cfg(target_os = "windows")]
fn install_keys(view: &Webview) -> tauri::Result<()> {
    use webview2_com::{AcceleratorKeyPressedEventHandler, Microsoft::Web::WebView2::Win32::*};
    let window = view.window();
    view.with_webview(move |platform| unsafe {
        let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let mut key = 0;
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN;
                let mut status = COREWEBVIEW2_PHYSICAL_KEY_STATUS::default();
                args.VirtualKey(&mut key)?;
                args.KeyEventKind(&mut kind)?;
                args.PhysicalKeyStatus(&mut status)?;
                let close = key == 0x73 && status.IsMenuKeyDown.as_bool();
                let fullscreen = key == 0x7A;
                if close || fullscreen {
                    args.SetHandled(true)?;
                    if !status.WasKeyDown.as_bool()
                        && (kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                            || kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN)
                    {
                        let w = window.clone();
                        // COM blocks the WebView during this callback: defer window operations.
                        let _ = window.app_handle().run_on_main_thread(move || {
                            report(if close { w.close() } else { toggle(&w) });
                        });
                    }
                }
            }
            Ok(())
        }));
        let mut token = 0;
        if let Err(e) = platform
            .controller()
            .add_AcceleratorKeyPressed(&handler, &mut token)
        {
            eprintln!("Could not install host shortcuts: {e}");
        }
    })
}
#[cfg(not(target_os = "windows"))]
fn install_keys(_: &Webview) -> tauri::Result<()> {
    Ok(())
}
fn report(result: tauri::Result<()>) {
    if let Err(e) = result {
        eprintln!("Window operation failed: {e}");
    }
}

// The hidden overlay receives no mouse input. Detect the 7-shaped edge band in
// native window coordinates, then reveal the original rectangular control view.
fn start_edge_watcher(app: tauri::AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(60));
        let Some(w) = app.get_window("main") else {
            break;
        };
        if w.state::<ConfirmationOpen>().0.load(Ordering::Relaxed)
            || !w.is_fullscreen().unwrap_or(false)
            || !w.is_focused().unwrap_or(false)
            || w.state::<ControlsOpen>().0.load(Ordering::Relaxed)
        {
            continue;
        }
        if let (Ok(cursor), Ok(origin), Ok(size), Ok(scale)) = (
            w.cursor_position(),
            w.inner_position(),
            w.inner_size(),
            w.scale_factor(),
        ) {
            let right = (f64::from(origin.x) + f64::from(size.width) - cursor.x) / scale;
            let top = (cursor.y - f64::from(origin.y)) / scale;
            if (0. ..=176.).contains(&right)
                && (0. ..=64.).contains(&top)
                && (top <= 10. || right <= 10.)
            {
                let target = w.clone();
                let _ = w.run_on_main_thread(move || {
                    target
                        .state::<ControlsOpen>()
                        .0
                        .store(true, Ordering::Relaxed);
                    report(sync_controls(&target));
                });
            }
        }
    });
}

fn sync_confirmation(w: &Window) -> tauri::Result<()> {
    if let Some(dialog) = w.app_handle().get_webview("exit-confirm") {
        let size = w.inner_size()?.to_logical::<f64>(w.scale_factor()?);
        if size.width > 0. && size.height > 0. {
            dialog.set_size(size)?;
        }
        if w.state::<ConfirmationOpen>().0.load(Ordering::Relaxed) {
            dialog.show()?;
        } else {
            dialog.hide()?;
        }
    }
    Ok(())
}

fn show_exit_confirmation(w: &Window) -> tauri::Result<()> {
    w.state::<ConfirmationOpen>()
        .0
        .store(true, Ordering::Relaxed);
    // Block all parent-window actions with a full-client-area local view, rather
    // than disabling the parent HWND (which would also disable this child).
    sync_confirmation(w)?;
    if let Some(dialog) = w.app_handle().get_webview("exit-confirm") {
        dialog.set_focus()?;
        dialog.eval("document.querySelector('#cancel')?.focus()")?;
    }
    Ok(())
}
fn focus_game(w: &Window) -> tauri::Result<()> {
    w.set_focus()?;
    if let Some(game) = w.app_handle().get_webview("game") {
        game.set_focus()?;
    }
    Ok(())
}
