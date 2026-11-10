#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod feedback;
mod media;
mod preferences;
#[cfg(windows)]
mod record_audio;
mod record_control;
#[cfg(windows)]
mod record_video;
#[cfg(windows)]
mod recorder;
#[cfg(windows)]
mod steam_input;
#[cfg(windows)]
mod steam_probe;
#[cfg(windows)]
mod steam_snapshot;
#[cfg(windows)]
mod window_geometry;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tauri::{
    webview::NewWindowResponse, LogicalPosition, LogicalSize, Manager, Webview, WebviewBuilder,
    WebviewUrl, WebviewWindowBuilder, Window,
};
const TITLE: &str = "云·星穹铁道";
const URL: &str = "https://sr.mihoyo.com/cloud/#/";
// Keep editable controls usable while disabling browser menus and incidental text selection.
const INTERACTION_SCRIPT: &str = r#"(()=>{
  window.addEventListener('contextmenu',event=>event.preventDefault(),true);
  const install=()=>{
    if(document.getElementById('cloud-starrail-interaction-style'))return;
    const style=document.createElement('style');style.id='cloud-starrail-interaction-style';
    style.textContent='*{-webkit-user-select:none!important;user-select:none!important}input,textarea,[contenteditable],[contenteditable] *{-webkit-user-select:text!important;user-select:text!important}';
    (document.head||document.documentElement).appendChild(style);
  };
  if(document.readyState==='loading')document.addEventListener('DOMContentLoaded',install,{once:true});else install();
})()"#;

struct ControlsOpen(AtomicBool);
#[derive(Default)]
struct SettingsAnchor(std::sync::Mutex<[f64; 4]>);
struct ExitApproved(AtomicBool);
struct ExitWaiting(AtomicBool);
struct ConfirmationOpen(AtomicBool);
static POPUP: AtomicUsize = AtomicUsize::new(0);

fn main() {
    #[cfg(windows)]
    if std::env::args_os().any(|argument| argument == "--steam-probe") {
        steam_probe::run();
        return;
    }
    let mut context = tauri::generate_context!();
    if std::env::args_os().any(|a| a == "--record-smoke") {
        context.config_mut().identifier.push_str(".record-smoke");
    }
    if std::env::args_os().any(|a| a == "--record-dev") {
        context.config_mut().identifier.push_str(".record-dev");
    }
    tauri::Builder::default()
        .manage(window_geometry::Aspect::default())
        .manage(recorder::Recorder::default())
        .manage(ControlsOpen(AtomicBool::new(false)))
        .manage(SettingsAnchor::default())
        .manage(ExitApproved(AtomicBool::new(false)))
        .manage(ExitWaiting(AtomicBool::new(false)))
        .manage(ConfirmationOpen(AtomicBool::new(false)))
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(w) = app.get_window("main") {
                report(w.unminimize());
                report(w.show());
                report(w.set_focus());
            }
        }))
        .invoke_handler(tauri::generate_handler![window_action, media_settings])
        .setup(|app| {
            app.manage(preferences::Preferences::load(app.handle())?);
            let profile = app.path().app_local_data_dir()?.join("WebView2");
            std::fs::create_dir_all(&profile)?;
            let main = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title(TITLE)
                .initialization_script(INTERACTION_SCRIPT)
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
            window_geometry::install(&native)?;
            restore_placement(&native)?;
            let popup_app = app.handle().clone();
            let popup_profile = profile.clone();
            let game = native.add_child(
                WebviewBuilder::new(
                    "game",
                    if std::env::args_os().any(|a| a == "--record-smoke") {
                        WebviewUrl::App("record-test.html".into())
                    } else {
                        WebviewUrl::External(URL.parse()?)
                    },
                )
                .initialization_script(INTERACTION_SCRIPT)
                .additional_browser_args(if std::env::args_os().any(|a| a == "--record-smoke") {
                    "--autoplay-policy=no-user-gesture-required"
                } else {
                    ""
                })
                .data_directory(profile)
                .disable_drag_drop_handler()
                .on_navigation(|url| matches!(url.scheme(), "https" | "http"))
                .on_new_window(move |url, features| {
                    if !matches!(url.scheme(), "https" | "http") && url.as_str() != "about:blank" {
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
                    .initialization_script(INTERACTION_SCRIPT)
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
                    .initialization_script(INTERACTION_SCRIPT)
                    .transparent(true)
                    .background_color(tauri::webview::Color(0, 0, 0, 0)),
                LogicalPosition::new(1104., 0.),
                LogicalSize::new(176., 40.),
            )?;
            // A child view is clipped by the actual main-window boundary, including
            // DWM corners. Showing it does not run a native window opening animation.
            let confirmation = native.add_child(
                WebviewBuilder::new("exit-confirm", WebviewUrl::App("confirm.html".into()))
                    .initialization_script(INTERACTION_SCRIPT)
                    .transparent(true)
                    .background_color(tauri::webview::Color(0, 0, 0, 0)),
                LogicalPosition::new(0., 0.),
                LogicalSize::new(1280., 760.),
            )?;
            confirmation.hide()?;
            let settings =
                WebviewWindowBuilder::new(app, "settings", WebviewUrl::App("settings.html".into()))
                    .title("截图与录屏 · 云·星穹铁道")
                    .initialization_script(INTERACTION_SCRIPT)
                    .decorations(false)
                    .inner_size(460., 600.)
                    .resizable(false)
                    .skip_taskbar(true)
                    .visible(false)
                    .parent(&main)?
                    .build()?;
            settings.with_webview(|platform| unsafe {
                use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
                use windows::core::Interface;
                let result = platform
                    .controller()
                    .CoreWebView2()
                    .and_then(|v| v.Settings())
                    .and_then(|v| {
                        v.SetAreDefaultContextMenusEnabled(false)?;
                        v.cast::<ICoreWebView2Settings3>()
                    })
                    .and_then(|v| v.SetAreBrowserAcceleratorKeysEnabled(false));
                if let Err(e) = result {
                    eprintln!("Cannot disable settings browser accelerators: {e}");
                }
            })?;
            settings.on_window_event({
                let app = app.handle().clone();
                move |event| {
                    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        if let Some(v) = app.get_webview_window("settings") {
                            let _ = v.hide();
                        }
                        app.state::<preferences::Preferences>()
                            .editing
                            .store(false, Ordering::Relaxed);
                    }
                }
            });
            install_keys(&confirmation)?;
            install_keys(&game)?;
            if let Some(chrome) = app.get_webview("controls") {
                install_keys(&chrome)?;
            }
            if let Some(shell) = app.get_webview("main") {
                install_keys(&shell)?;
            }
            layout(&native)?;
            feedback::warm();
            main.show()?;
            start_edge_watcher(app.handle().clone());
            recorder::watch(native.clone());
            #[cfg(windows)]
            if std::env::args_os().any(|argument| {
                argument == "--steam"
                    || argument == "--steam-isolate"
                    || argument == "--steam-input-test"
                    || argument == "--steam-web-overlay"
            }) {
                steam_probe::start_on_main(native);
            }
            Ok(())
        })
        .on_window_event(|w, event| {
            if w.label() != "main" {
                return;
            }
            match event {
                tauri::WindowEvent::Resized(_) => report(layout(w)),
                tauri::WindowEvent::ScaleFactorChanged { .. } => {
                    window_geometry::update_scale(w);
                    report(layout(w));
                }
                tauri::WindowEvent::CloseRequested { api, .. } => {
                    if w.state::<ExitApproved>().0.load(Ordering::Relaxed) {
                        w.state::<preferences::Preferences>().save_window(w);
                    }
                    if w.state::<ExitApproved>().0.load(Ordering::Relaxed) && defer_pending_exit(w)
                    {
                        api.prevent_close();
                    } else if !w.state::<ExitApproved>().0.load(Ordering::Relaxed) {
                        api.prevent_close();
                        report(show_exit_confirmation(w));
                    }
                }
                tauri::WindowEvent::Destroyed => w.app_handle().exit(0),
                _ => {}
            }
        })
        .run(context)
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
        "screenshot" => {
            media::screenshot(w.clone());
            Ok(())
        }
        "settings" => show_settings(&w),
        "record_toggle" => {
            w.state::<recorder::Recorder>().toggle(w.clone());
            Ok(())
        }
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
        let (ratio_width, ratio_height) = window_geometry::initialize(w, 16, 9);
        let area = m.work_area();
        // Preserve the GAME viewport ratio; the title is extra client height.
        let top = (40.0 * w.scale_factor()?).round();
        let factor = (f64::from(area.size.width) * 0.82 / f64::from(ratio_width))
            .min((f64::from(area.size.height) * 0.82 - top).max(1.0) / f64::from(ratio_height));
        let width = (f64::from(ratio_width) * factor).round() as u32;
        let height = (f64::from(ratio_height) * factor + top).round() as u32;
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
    w.state::<ControlsOpen>().0.store(false, Ordering::Relaxed);
    if !fullscreen {
        w.state::<preferences::Preferences>().save_window(w);
    }
    w.set_fullscreen(!fullscreen)?;
    window_geometry::update_scale(w);
    if fullscreen {
        restore_placement(w)?;
    }
    layout(w)?;
    focus_game(w)
}

fn layout(w: &Window) -> tauri::Result<()> {
    #[cfg(windows)]
    if steam_probe::webviews_hidden() {
        return Ok(());
    }
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
    if let Some(view) = w.app_handle().get_webview("main") {
        view.eval(format!("window.setShellFullscreen?.({fullscreen})"))?;
    }
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
    let relay_only = view.label() == "exit-confirm";
    let window = view.window();
    view.with_webview(move |platform| unsafe {
        if let Err(e) = platform
            .controller()
            .CoreWebView2()
            .and_then(|v| v.Settings())
            .and_then(|v| v.SetAreDefaultContextMenusEnabled(false))
        {
            eprintln!("Cannot disable browser context menu: {e}");
        }
        let handler = AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
            if let Some(args) = args {
                let mut key = 0;
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN;
                let mut status = COREWEBVIEW2_PHYSICAL_KEY_STATUS::default();
                args.VirtualKey(&mut key)?;
                args.KeyEventKind(&mut kind)?;
                args.PhysicalKeyStatus(&mut status)?;
                steam_input::observe_web_key(
                    key,
                    kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                        || kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
                );
                if !window.state::<preferences::Preferences>().editing() {
                    let config = window.state::<preferences::Preferences>().settings();
                    if key == config.screenshot_key.key
                        && (kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_UP
                            || kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_UP)
                    {
                        media::clear_screenshot_key();
                    }
                    if config.record_key.matches(key) || config.screenshot_key.matches(key) {
                        if config.screenshot_key.matches(key)
                            && !status.WasKeyDown.as_bool()
                            && (kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                                || kind == COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN)
                        {
                            media::observe_screenshot_key();
                        }
                        args.SetHandled(true)?;
                        // The native watcher executes edge-triggered shortcuts for every view.
                        return Ok(());
                    }
                }
                if steam_input::is_web_toggle(key) {
                    args.SetHandled(true)?;
                    return Ok(());
                }
                if relay_only {
                    return Ok(());
                }
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
        if steam_probe::webviews_hidden()
            || w.state::<ConfirmationOpen>().0.load(Ordering::Relaxed)
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

fn restore_placement(w: &Window) -> tauri::Result<()> {
    let saved = w
        .state::<preferences::Preferences>()
        .data
        .lock()
        .unwrap()
        .window
        .clone();
    if let Some(saved) = saved {
        if saved.width >= 480. {
            let monitors = w.available_monitors()?;
            let monitor = monitors
                .iter()
                .find(|m| {
                    let a = m.work_area();
                    saved.x >= a.position.x
                        && saved.y >= a.position.y
                        && saved.x < a.position.x + a.size.width as i32
                        && saved.y < a.position.y + a.size.height as i32
                })
                .cloned()
                .or(w.primary_monitor()?);
            if let Some(m) = monitor {
                let scale = m.scale_factor();
                let area = m.work_area();
                let width = (saved.width * scale)
                    .min(area.size.width as f64)
                    .min((area.size.height as f64 - 40. * scale).max(1.) * 16. / 9.);
                let height = width * 9. / 16. + 40. * scale;
                w.set_size(tauri::PhysicalSize::new(
                    width.round() as u32,
                    height.round() as u32,
                ))?;
                w.set_position(tauri::PhysicalPosition::new(
                    saved.x.clamp(
                        area.position.x,
                        area.position.x + (area.size.width as f64 - width).max(0.) as i32,
                    ),
                    saved.y.clamp(
                        area.position.y,
                        area.position.y + (area.size.height as f64 - height).max(0.) as i32,
                    ),
                ))?;
            }
        }
        // Restore fullscreen only during startup; toggle() has already left fullscreen.
        if saved.fullscreen && !w.is_visible()? {
            w.set_fullscreen(true)?;
        }
        window_geometry::update_scale(w);
    }
    Ok(())
}
fn show_settings(w: &Window) -> tauri::Result<()> {
    if w.is_fullscreen()? {
        return Ok(());
    }
    if let Some(view) = w.app_handle().get_webview_window("settings") {
        if view.is_visible()? {
            return view.eval("window.closeSettings?.()");
        }
        let origin = w.inner_position()?;
        let scale = w.scale_factor()?;
        let mut x = origin.x + (16. * scale) as i32;
        let mut y = origin.y + (44. * scale) as i32;
        if let Some(m) = w.current_monitor()? {
            let area = m.work_area();
            let height = (view.inner_size()?.height as f64)
                .min(area.size.height as f64 - 16.)
                .max(200.);
            view.set_size(tauri::PhysicalSize::new(
                (460. * scale).round() as u32,
                height.round() as u32,
            ))?;
            x = x.clamp(
                area.position.x,
                area.position.x + (area.size.width as f64 - 460. * scale).max(0.) as i32,
            );
            y = y.clamp(
                area.position.y,
                area.position.y + (area.size.height as f64 - height).max(0.) as i32,
            );
        }
        view.set_position(tauri::PhysicalPosition::new(x, y))?;
        view.show()?;
        view.set_focus()?;
        view.eval("window.loadSettings?.()")?;
    }
    Ok(())
}
fn trusted_local(view: &Webview) -> Result<(), String> {
    if !matches!(view.label(), "settings" | "main") {
        return Err("Untrusted view".into());
    }
    let url = view.url().map_err(|e| e.to_string())?;
    if url.scheme() != "tauri"
        && !matches!(
            url.host_str(),
            Some("tauri.localhost" | "localhost" | "127.0.0.1")
        )
    {
        return Err("Untrusted origin".into());
    }
    Ok(())
}
#[tauri::command]
async fn media_settings(
    webview: Webview,
    action: String,
    settings: Option<preferences::Settings>,
    height: Option<f64>,
    anchor: Option<[f64; 4]>,
) -> Result<serde_json::Value, String> {
    trusted_local(&webview)?;
    let app = webview.app_handle();
    let state = app.state::<preferences::Preferences>();
    match action.as_str() {
        "anchor" => {
            if webview.label() != "main" {
                return Err("Untrusted view".into());
            }
            let bounds = anchor
                .filter(|a| a.iter().all(|v| v.is_finite()))
                .ok_or("Invalid anchor")?;
            *app.state::<SettingsAnchor>().0.lock().unwrap() = bounds;
            Ok(serde_json::json!(true))
        }
        "resize" => {
            if webview.label() != "settings" {
                return Err("Untrusted view".into());
            }
            let requested = height
                .filter(|h| h.is_finite())
                .ok_or("Invalid height")?
                .clamp(200., 900.);
            let w = webview.window();
            let max = w
                .current_monitor()
                .map_err(|e| e.to_string())?
                .map(|m| m.work_area().size.height as f64 / m.scale_factor() - 16.)
                .unwrap_or(800.);
            let desired = requested.min(max).max(200.);
            w.set_size(tauri::LogicalSize::new(460., desired))
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!(desired))
        }
        "dismiss" => {
            // Clicking the anchor is handled by its pointer-down toggle. A blur
            // arriving first must not hide then reopen the same popup.
            if let Some(main) = app.get_window("main") {
                if settings_anchor_active(&main) {
                    return Ok(serde_json::json!(true));
                }
            }
            state.editing.store(false, Ordering::Relaxed);
            webview.window().hide().map_err(|e| e.to_string())?;
            Ok(serde_json::json!(true))
        }
        "drag" => {
            webview
                .window()
                .start_dragging()
                .map_err(|e| e.to_string())?;
            Ok(serde_json::json!(true))
        }
        "get" => serde_json::to_value(state.settings()).map_err(|e| e.to_string()),
        "save" => {
            let mut settings = settings.ok_or("Missing settings")?;
            settings.validate()?;
            let mut data = state.data.lock().unwrap();
            let mut updated = data.clone();
            updated.settings = settings;
            state.commit(&updated)?;
            *data = updated;
            Ok(serde_json::json!(true))
        }
        "editing" => {
            state.editing.store(true, Ordering::Relaxed);
            Ok(serde_json::json!(true))
        }
        "edited" => {
            state.editing.store(false, Ordering::Relaxed);
            Ok(serde_json::json!(true))
        }
        "close" => {
            state.editing.store(false, Ordering::Relaxed);
            webview.window().hide().map_err(|e| e.to_string())?;
            if let Some(w) = app.get_window("main") {
                focus_game(&w).map_err(|e| e.to_string())?;
            }
            Ok(serde_json::json!(true))
        }
        "open_folder" => {
            if webview.label() != "settings" {
                return Err("Untrusted view".into());
            }
            let folder = state.settings().folder;
            tauri::async_runtime::spawn_blocking(move || media::open_folder(&folder))
                .await
                .map_err(|e| e.to_string())??;
            Ok(serde_json::json!(true))
        }
        "folder" => {
            let w = webview.window();
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            app.run_on_main_thread(move || {
                let _ = tx.send(media::choose_folder(&w));
            })
            .map_err(|e| e.to_string())?;
            tauri::async_runtime::spawn_blocking(move || rx.recv().map_err(|e| e.to_string())?)
                .await
                .map_err(|e| e.to_string())?
                .map(|v| serde_json::json!(v))
        }
        _ => Err("Unknown settings action".into()),
    }
}

fn defer_pending_exit(w: &Window) -> bool {
    media::close_screenshots();
    let recorder = w.state::<recorder::Recorder>();
    recorder.prepare_close();
    if !recorder.busy() && media::pending_screenshots() == 0 && feedback::pending() == 0 {
        return false;
    }
    if !w.state::<ExitWaiting>().0.swap(true, Ordering::AcqRel) {
        let w = w.clone();
        std::thread::spawn(move || {
            while w.state::<recorder::Recorder>().busy()
                || media::pending_screenshots() > 0
                || feedback::pending() > 0
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            w.app_handle().exit(0);
        });
    }
    true
}

fn settings_anchor_active(w: &Window) -> bool {
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::GetForegroundWindow};
    if !w
        .hwnd()
        .is_ok_and(|h| unsafe { GetForegroundWindow() == HWND(h.0) })
    {
        return false;
    }
    let (Ok(cursor), Ok(origin), Ok(scale)) =
        (w.cursor_position(), w.inner_position(), w.scale_factor())
    else {
        return false;
    };
    let [left, top, right, bottom] = *w.state::<SettingsAnchor>().0.lock().unwrap();
    let x = (cursor.x - f64::from(origin.x)) / scale;
    let y = (cursor.y - f64::from(origin.y)) / scale;
    left < right && top < bottom && x >= left && x < right && y >= top && y < bottom
}
