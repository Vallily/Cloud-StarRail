//! Native window recorder: WGC -> D3D11 -> Media Foundation H.264/AAC MP4.
use std::{
    fs::OpenOptions,
    io::Write,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Manager, Window};
use windows::{
    core::{factory, Interface, GUID, HSTRING},
    Graphics::{
        Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem, GraphicsCaptureSession},
        DirectX::{Direct3D11::IDirect3DDevice, DirectXPixelFormat},
    },
    Win32::{
        Foundation::{HMODULE, HWND},
        Graphics::{
            Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0},
            Direct3D11::*,
            Dxgi::*,
        },
        Media::MediaFoundation::*,
        System::{
            Com::{CoTaskMemFree, IPersist},
            Performance::{QueryPerformanceCounter, QueryPerformanceFrequency},
            WinRT::{
                Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess},
                Graphics::Capture::IGraphicsCaptureItemInterop,
                RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED,
            },
        },
        UI::{
            Input::KeyboardAndMouse::GetAsyncKeyState,
            WindowsAndMessaging::{GetForegroundWindow, IsIconic},
        },
    },
};
type AnyResult<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
#[derive(Clone, Default)]
pub struct Recorder(Arc<Inner>);
#[derive(Default)]
struct Inner {
    busy: AtomicBool,
    stop: AtomicBool,
    control: Mutex<crate::record_control::Control>,
    text: Mutex<String>,
}
impl Recorder {
    pub fn busy(&self) -> bool {
        self.0.busy.load(Ordering::Relaxed)
    }
    pub fn status(&self) -> String {
        self.0.text.lock().unwrap().clone()
    }
    pub fn set(&self, text: impl Into<String>) {
        *self.0.text.lock().unwrap() = text.into();
    }
    fn request_stop(&self) {
        self.0.control.lock().unwrap().stop();
        self.0.stop.store(true, Ordering::Release);
    }
    pub fn prepare_close(&self) {
        self.0.control.lock().unwrap().close();
        self.0.stop.store(true, Ordering::Release);
        if self.busy() {
            self.set("正在保存录像，完成后退出…");
        }
    }
    pub fn active(&self) -> bool {
        self.0.control.lock().unwrap().phase == crate::record_control::Phase::Recording
    }
    fn wanted(&self) -> bool {
        self.0.control.lock().unwrap().wanted()
    }
    fn started(&self) -> bool {
        let mut c = self.0.control.lock().unwrap();
        if c.started() {
            self.set("录制中");
            true
        } else {
            false
        }
    }
    fn stopping(&self, minimized: bool) {
        self.0.control.lock().unwrap().stopping(minimized);
        self.set("正在保存录像…");
    }
    pub fn toggle(&self, window: Window) {
        let mut control = self.0.control.lock().unwrap();
        let spawn_worker = control.toggle();
        self.0.stop.store(!control.wanted(), Ordering::Release);
        if !spawn_worker {
            if self.busy() {
                self.set(if control.wanted() {
                    "正在处理上一段，随后开始录制…"
                } else {
                    "正在停止录制…"
                });
            }
            return;
        }
        self.0.busy.store(true, Ordering::Release);
        self.set("正在启动录制…");
        drop(control);
        let state = self.clone();
        if let Err(e) = std::thread::Builder::new()
            .name("window-recorder".into())
            .spawn(move || loop {
                let result = unsafe { record(&window, &state) };
                if let Err(e) = &result {
                    state.set(format!("录制失败：{e}"));
                    crate::media::notify(&window, &format!("录制失败：{e}"));
                }
                let mut control = state.0.control.lock().unwrap();
                if control.finish(result.is_ok()) {
                    state.0.stop.store(false, Ordering::Release);
                    state.set("正在启动录制…");
                } else {
                    state.0.busy.store(false, Ordering::Release);
                    break;
                }
            })
        {
            let mut control = self.0.control.lock().unwrap();
            control.finish(false);
            self.0.busy.store(false, Ordering::Release);
            self.set(format!("无法启动录制：{e}"));
        }
    }
}

pub fn watch(window: Window) {
    std::thread::spawn(move || {
        let smoke = std::env::args_os().any(|a| a == "--record-smoke");
        let mut last_keys = [false; 256];
        unsafe extern "C" {
            fn xbox_share_open(hwnd: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
            fn xbox_share_take(ptr: *mut std::ffi::c_void) -> bool;
            fn xbox_share_close(ptr: *mut std::ffi::c_void);
        }
        let share =
            unsafe { xbox_share_open(window.hwnd().map(|h| h.0).unwrap_or(std::ptr::null_mut())) };
        eprintln!(
            "Xbox GameInput share listener available={}",
            !share.is_null()
        );
        let mut last_text = String::new();
        let mut last_busy = false;
        let start = Instant::now();
        let mut smoke_started = false;
        let resize_smoke = std::env::args_os().any(|a| a == "--record-resize-smoke");
        let mut resize_stage = 0u32;
        let stress = smoke && std::env::args_os().any(|a| a == "--media-stress-smoke");
        let mut stress_stage = 0;
        while window.app_handle().get_window("main").is_some() {
            let state = window.state::<Recorder>();
            let foreground = window
                .hwnd()
                .is_ok_and(|h| unsafe { GetForegroundWindow() == HWND(h.0) });
            let prefs = window.state::<crate::preferences::Preferences>();
            let config = prefs.settings();
            let enabled = foreground
                && !prefs.editing()
                && !window
                    .state::<crate::ConfirmationOpen>()
                    .0
                    .load(Ordering::Relaxed);
            let record_down = unsafe { GetAsyncKeyState(config.record_key.key as i32) < 0 };
            let shot_down = unsafe { GetAsyncKeyState(config.screenshot_key.key as i32) < 0 };
            let record_edge = record_down && !last_keys[config.record_key.key as usize];
            let shot_edge = shot_down && !last_keys[config.screenshot_key.key as usize];
            if enabled {
                if record_edge && config.record_key.modifiers() {
                    state.toggle(window.clone());
                } else if shot_edge && config.screenshot_key.modifiers() {
                    crate::media::screenshot_from(window.clone(), "keyboard");
                }
            }
            last_keys[config.record_key.key as usize] = record_down;
            last_keys[config.screenshot_key.key as usize] = shot_down;
            if unsafe { xbox_share_take(share) } && enabled {
                crate::media::screenshot_from(window.clone(), "xbox-share");
            }
            if stress {
                let elapsed = start.elapsed().as_millis();
                let thresholds = [5000, 8000, 8050, 8100, 8150, 11000, 11020, 14000];
                if stress_stage < thresholds.len() && elapsed >= thresholds[stress_stage] {
                    match stress_stage {
                        0 => {
                            for _ in 0..3 {
                                crate::media::screenshot_from(window.clone(), "stress-smoke");
                            }
                            state.toggle(window.clone());
                        }
                        7 => {
                            window
                                .state::<crate::ExitApproved>()
                                .0
                                .store(true, Ordering::Release);
                            let _ = window.close();
                        }
                        _ => state.toggle(window.clone()),
                    }
                    stress_stage += 1;
                }
            }
            if smoke && !stress && !smoke_started && start.elapsed() > Duration::from_secs(5) {
                state.toggle(window.clone());
                smoke_started = true;
            }
            if smoke
                && !stress
                && smoke_started
                && start.elapsed() > Duration::from_secs(if resize_smoke { 20 } else { 17 })
                && state.busy()
            {
                state.request_stop();
            }
            if smoke && resize_smoke {
                let seconds = start.elapsed().as_secs_f64();
                if (7.0..12.0).contains(&seconds) {
                    let width = (1500.0
                        + 600.0 * ((seconds - 7.0) * std::f64::consts::TAU / 3.0).sin())
                    .round() as u32;
                    let top = (40.0 * window.scale_factor().unwrap_or(1.0)).round() as u32;
                    let height = (f64::from(width) * 9.0 / 16.0).round() as u32 + top;
                    let _ = window.set_size(tauri::PhysicalSize::new(width, height));
                    resize_stage = 1;
                } else if seconds >= 13.0 && resize_stage < 2 {
                    let _ = crate::toggle(&window);
                    resize_stage = 2;
                } else if seconds >= 16.0 && resize_stage < 3 {
                    let _ = crate::toggle(&window);
                    resize_stage = 3;
                }
            }
            let text = state.status();
            let active = state.active();
            if text != last_text || active != last_busy {
                if let Some(view) = window.app_handle().get_webview("main") {
                    let _ = view.eval(format!(
                        "window.setRecordingStatus?.({}, {})",
                        serde_json::to_string(&text).unwrap(),
                        active
                    ));
                }
                last_text = text;
                last_busy = active;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
        unsafe {
            xbox_share_close(share);
        }
    });
}
struct Runtime;
impl Drop for Runtime {
    fn drop(&mut self) {
        unsafe {
            eprintln!("record runtime: MF shutdown");
            let _ = MFShutdown();
            eprintln!("record runtime: WinRT uninitialize");
            RoUninitialize();
        }
    }
}
struct Capture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    stopped: bool,
}
impl Capture {
    fn stop(&mut self) {
        if !self.stopped {
            let _ = self.session.Close();
            let _ = self.pool.Close();
            self.stopped = true;
        }
    }
}
impl Drop for Capture {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Writer {
    sink: IMFSinkWriter,
    finished: bool,
}
impl Writer {
    unsafe fn finish(&mut self) -> windows::core::Result<()> {
        if !self.finished {
            self.sink.Finalize()?;
            self.finished = true;
        }
        Ok(())
    }
}
impl Drop for Writer {
    fn drop(&mut self) {
        eprintln!("writer: dropping, finalized={}", self.finished);
        if !self.finished {
            unsafe {
                let _ = self.sink.Finalize();
            }
        }
    }
}
struct Log(std::path::PathBuf);
impl Log {
    fn line(&self, text: impl std::fmt::Display) {
        eprintln!("record: {text}");
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&self.0) {
            let _ = writeln!(file, "{text}");
        }
    }
}
unsafe fn ticks() -> windows::core::Result<i64> {
    let (mut count, mut frequency) = (0, 0);
    QueryPerformanceCounter(&mut count)?;
    QueryPerformanceFrequency(&mut frequency)?;
    Ok(((count as i128) * 10_000_000 / (frequency as i128)) as i64)
}
fn packed(a: u32, b: u32) -> u64 {
    ((a as u64) << 32) | b as u64
}
unsafe fn string_attr(attrs: &IMFAttributes, key: &GUID) -> Option<String> {
    let (mut value, mut length) = (windows::core::PWSTR::null(), 0);
    attrs
        .GetAllocatedString(key, &mut value, &mut length)
        .ok()?;
    let text = value.to_string().ok();
    CoTaskMemFree(Some(value.0 as *const _));
    text
}
unsafe fn video_type(
    subtype: GUID,
    width: u32,
    height: u32,
) -> windows::core::Result<IMFMediaType> {
    let t = MFCreateMediaType()?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    t.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
    t.SetUINT64(&MF_MT_FRAME_SIZE, packed(width, height))?;
    t.SetUINT64(&MF_MT_FRAME_RATE, packed(60, 1))?;
    t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, packed(1, 1))?;
    t.SetUINT32(&MF_MT_INTERLACE_MODE, 2)?;
    Ok(t)
}
unsafe fn audio_type(subtype: GUID) -> windows::core::Result<IMFMediaType> {
    let t = MFCreateMediaType()?;
    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
    t.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
    t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, 2)?;
    t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, 48000)?;
    t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
    Ok(t)
}
unsafe fn audio_sample(
    sink: &IMFSinkWriter,
    stream: u32,
    bytes: &[u8],
    position: u64,
) -> windows::core::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let buffer = MFCreateMemoryBuffer(bytes.len() as u32)?;
    let mut ptr = std::ptr::null_mut();
    buffer.Lock(&mut ptr, None, None)?;
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
    buffer.Unlock()?;
    buffer.SetCurrentLength(bytes.len() as u32)?;
    let sample = MFCreateSample()?;
    sample.AddBuffer(&buffer)?;
    sample.SetSampleTime((position * 10_000_000 / 48000) as i64)?;
    sample.SetSampleDuration(((bytes.len() as u64 / 4) * 10_000_000 / 48000) as i64)?;
    sink.WriteSample(stream, &sample)
}
unsafe fn record(window: &Window, state: &Recorder) -> AnyResult<()> {
    if !state.wanted() {
        state.set("已取消录制");
        return Ok(());
    }
    RoInitialize(RO_INIT_MULTITHREADED)?;
    MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
    let _runtime = Runtime;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let config = window.state::<crate::preferences::Preferences>().settings();
    let folder = config.folder.clone();
    std::fs::create_dir_all(&folder)?;
    let path = folder.join(format!("StarRail-{stamp}-{}.mp4", std::process::id()));
    if path.exists() {
        return Err("录像文件已存在".into());
    }
    let log = Log(path.with_extension("log"));
    log.line(format!(
        "Cloud-StarRail {} WGC -> MF H264/AAC",
        env!("CARGO_PKG_VERSION")
    ));
    let mut started_feedback = false;
    let mut stopped_feedback = false;
    let mut cancelled = false;
    let result = (|| -> AnyResult<()> {
        let pid = browser_pid(window)?;
        log.line(format!("audio target browser process tree={pid}"));
        let mut audio = crate::record_audio::AudioCapture::new(pid)
            .map_err(|e| format!("无法只录制游戏声音：{e}"))?;
        log.line("audio client initialized");
        let (mut device, mut context) = (None, None);
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let device = device.ok_or("D3D device missing")?;
        let context = context.ok_or("D3D context missing")?;
        let multithread: ID3D11Multithread = context.cast()?;
        let _ = multithread.SetMultithreadProtected(true);
        log.line("D3D device initialized");
        let dxgi: IDXGIDevice = device.cast()?;
        let winrt: IDirect3DDevice = CreateDirect3D11DeviceFromDXGIDevice(&dxgi)?.cast()?;
        let interop: IGraphicsCaptureItemInterop =
            factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
        let native_hwnd = HWND(window.hwnd()?.0);
        let item: GraphicsCaptureItem = interop.CreateForWindow(native_hwnd)?;
        let mut size = item.Size()?;
        let initial_crop =
            capture_region(window, native_hwnd, size.Width as u32, size.Height as u32);
        let width = initial_crop.2;
        let height = initial_crop.3;
        if width == 0 || height == 0 {
            return Err("窗口尺寸不可录制".into());
        }
        // Grow in buckets; shrinking and in-capacity changes need no pool reset.
        let mut pool_size = size;
        pool_size.Width = capture_capacity(size.Width);
        pool_size.Height = capture_capacity(size.Height);
        let limit = if config.resolution == 0 {
            height
        } else {
            config.resolution
        };
        let factor = (limit as f64 / height as f64).min(1.);
        let out_width = ((width as f64 * factor) as u32) & !1;
        let out_height = ((height as f64 * factor) as u32) & !1;
        let mut token = 0;
        let mut manager = None;
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.ok_or("DXGI manager missing")?;
        manager.ResetDevice(&device, token)?;
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 5)?;
        let attrs = attrs.ok_or("MF attributes missing")?;
        attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
        attrs.SetUnknown(&MF_SINK_WRITER_D3D_MANAGER, &manager)?;
        attrs.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        log.line("creating sink writer");
        let sink = MFCreateSinkWriterFromURL(
            &HSTRING::from(path.to_string_lossy().as_ref()),
            None,
            &attrs,
        )?;
        log.line("sink writer initialized");
        let output = video_type(MFVideoFormat_H264, out_width, out_height)?;
        let bitrate = config.bitrate(limit);
        output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
        output.SetUINT32(&MF_MT_MPEG2_PROFILE, 100)?;
        let video_stream = sink.AddStream(&output)?;
        let mut converter = crate::record_video::Converter::new(
            &device,
            &context,
            pool_size.Width as u32,
            pool_size.Height as u32,
            out_width,
            out_height,
        )?;
        converter.set_source_size(width, height);
        let input = video_type(MFVideoFormat_NV12, out_width, out_height)?;
        sink.SetInputMediaType(video_stream, &input, None)?;
        log.line("video media types configured");
        let audio_out = audio_type(MFAudioFormat_AAC)?;
        audio_out.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 16000)?;
        audio_out.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
        audio_out.SetUINT32(&MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
        let audio_stream = sink.AddStream(&audio_out)?;
        let audio_in = audio_type(MFAudioFormat_PCM)?;
        audio_in.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)?;
        audio_in.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 192000)?;
        sink.SetInputMediaType(audio_stream, &audio_in, None)?;
        log.line("audio media types configured; begin writing");
        sink.BeginWriting()?;
        log.line("writing started");
        let mut writer = Writer {
            sink,
            finished: false,
        };
        let ex: IMFSinkWriterEx = writer.sink.cast()?;
        let mut hardware_verified = false;
        for index in 0..8 {
            let (mut category, mut transform) = (GUID::zeroed(), None);
            if ex
                .GetTransformForStream(video_stream, index, Some(&mut category), &mut transform)
                .is_err()
            {
                break;
            }
            if let Some(transform) = transform {
                let attrs = transform.GetAttributes()?;
                let name = string_attr(&attrs, &MFT_FRIENDLY_NAME_Attribute)
                    .unwrap_or_else(|| "unnamed".into());
                let hw = string_attr(&attrs, &MFT_ENUM_HARDWARE_URL_Attribute);
                if category == MFT_CATEGORY_VIDEO_ENCODER && hw.is_some() {
                    hardware_verified = true;
                }
                let clsid = transform.cast::<IPersist>().and_then(|p| p.GetClassID());
                log.line(format!("video transform {index} category={category:?} name={name} hardware_url={hw:?} clsid={clsid:?}"));
            }
        }
        log.line(format!("hardware_encoder_verified={hardware_verified}"));
        if !hardware_verified {
            return Err("未能确认硬件 H.264 编码器，本轮不回退到软件编码".into());
        }
        log.line(format!("input={width}x{height} output={out_width}x{out_height} fps=60 bitrate={bitrate}; hardware enabled, verify transforms above"));
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            3,
            pool_size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        session.SetIsCursorCaptureEnabled(false)?;
        // Border suppression is optional on old Windows; never fail a recording over it.
        match session.SetIsBorderRequired(false) {
            Ok(()) => log.line("capture border disabled"),
            Err(e) => log.line(format!("capture border could not be disabled: {e}")),
        }
        session.StartCapture()?;
        log.line("capture session started");
        let mut capture = Capture {
            pool,
            session,
            stopped: false,
        };
        let base = ticks()?;
        log.line("starting audio stream");
        audio.start()?;
        log.line("audio stream started");
        let start = Instant::now();
        let mut count = 0u64;
        let mut captured = 0u64;
        let mut last_report = Instant::now();
        let mut audio_position = 0u64;
        let mut latest = None;
        let mut input_dimensions = (width, height);
        let mut resize_count = 0u32;
        let mut pool_rebuilds = 0u32;
        let mut input_allocations = 0u32;
        let mut texture_capacity = (pool_size.Width as u32, pool_size.Height as u32);
        let mut discarded_queued_frames = 0u64;
        let mut audio_discontinuities = 0u64;
        let mut audio_gap_samples = 0u64;
        let mut max_video_us = 0u128;
        let mut max_pool_us = 0u128;
        let mut deadline = Instant::now();
        while window.app_handle().get_window("main").is_some() {
            // No Tauri/UI-thread request in the real-time recording loop:
            // Windows' modal resize loop may delay its event dispatcher.
            let minimized = IsIconic(native_hwnd).as_bool();
            let stopping = state.0.stop.load(Ordering::Relaxed) || minimized;
            if stopping {
                if minimized {
                    log.line("window minimized; finalizing current file");
                }
                capture.stop();
                audio.stop();
                state.stopping(minimized);
                if started_feedback && !stopped_feedback {
                    crate::media::sound("stop");
                    stopped_feedback = true;
                    log.line("STOP_FEEDBACK: capture stopped; file finalization pending");
                }
            }
            if !stopping {
                let mut newest = None;
                while let Ok(frame) = capture.pool.TryGetNextFrame() {
                    if let Some(old) = newest.replace(frame) {
                        let _ = old.Close();
                        discarded_queued_frames += 1;
                    }
                }
                if let Some(frame) = newest {
                    if captured == 0 {
                        log.line("first WGC frame received");
                    }
                    let current = frame.ContentSize()?;
                    if current.Width <= 1 || current.Height <= 1 {
                        frame.Close()?;
                    } else {
                        if current != size {
                            size = current;
                            resize_count += 1;
                            log.line(format!("SOURCE_RESIZE #{resize_count}: {}x{}; output fixed {out_width}x{out_height}",size.Width,size.Height));
                        }
                        if current.Width > pool_size.Width || current.Height > pool_size.Height {
                            frame.Close()?;
                            pool_size.Width = capture_capacity(current.Width.max(pool_size.Width));
                            pool_size.Height =
                                capture_capacity(current.Height.max(pool_size.Height));
                            let rebuild_started = Instant::now();
                            capture.pool.Recreate(
                                &winrt,
                                DirectXPixelFormat::B8G8R8A8UIntNormalized,
                                3,
                                pool_size,
                            )?;
                            max_pool_us = max_pool_us.max(rebuild_started.elapsed().as_micros());
                            pool_rebuilds += 1;
                            log.line(format!("POOL_GROW #{pool_rebuilds}: capacity {}x{}; audio capture thread continues",pool_size.Width,pool_size.Height));
                        } else {
                            let crop = capture_region(
                                window,
                                native_hwnd,
                                current.Width as u32,
                                current.Height as u32,
                            );
                            let source_width = crop.2;
                            let source_height = crop.3;
                            let surface = frame.Surface()?;
                            let access: IDirect3DDxgiInterfaceAccess = surface.cast()?;
                            let source: ID3D11Texture2D = access.GetInterface()?;
                            let mut desc = D3D11_TEXTURE2D_DESC::default();
                            source.GetDesc(&mut desc);
                            if desc.Width < crop.0 + source_width
                                || desc.Height < crop.1 + source_height
                            {
                                frame.Close()?;
                            } else {
                                let capacity = (pool_size.Width as u32, pool_size.Height as u32);
                                if latest.is_none() || texture_capacity != capacity {
                                    desc.Width = capacity.0;
                                    desc.Height = capacity.1;
                                    desc.MipLevels = 1;
                                    desc.ArraySize = 1;
                                    desc.BindFlags = 0;
                                    desc.MiscFlags = 0;
                                    desc.CPUAccessFlags = 0;
                                    desc.Usage = D3D11_USAGE_DEFAULT;
                                    let mut owned = None;
                                    device.CreateTexture2D(&desc, None, Some(&mut owned))?;
                                    let owned = owned.ok_or("capture copy missing")?;
                                    if texture_capacity != capacity {
                                        converter = crate::record_video::Converter::new(
                                            &device, &context, capacity.0, capacity.1, out_width,
                                            out_height,
                                        )?;
                                    }
                                    converter.set_source_size(source_width, source_height);
                                    input_dimensions = (source_width, source_height);
                                    texture_capacity = capacity;
                                    latest = Some(owned);
                                    input_allocations += 1;
                                    log.line(format!(
                                        "INPUT_ALLOCATION #{input_allocations}: capacity {}x{}",
                                        capacity.0, capacity.1
                                    ));
                                }
                                if input_dimensions != (source_width, source_height) {
                                    converter.set_source_size(source_width, source_height);
                                    input_dimensions = (source_width, source_height);
                                }
                                let area = D3D11_BOX {
                                    left: crop.0,
                                    top: crop.1,
                                    front: 0,
                                    right: crop.0 + source_width,
                                    bottom: crop.1 + source_height,
                                    back: 1,
                                };
                                // Only this thread uses the BGRA texture. The encoder
                                // receives separate NV12 textures, so reuse is safe;
                                // copy and conversion commands share one ordered context.
                                context.CopySubresourceRegion(
                                    latest.as_ref().unwrap(),
                                    0,
                                    0,
                                    0,
                                    0,
                                    &source,
                                    0,
                                    Some(&area),
                                );
                                frame.Close()?;
                                captured += 1;
                                if captured == 1 {
                                    log.line("first GPU texture copied");
                                }
                            }
                        }
                    }
                }
            }
            // Drain all captured PCM even when stop was requested during GPU work.
            while let Some(packet) = audio.read()? {
                if packet.discontinuity && audio_position > 0 {
                    audio_discontinuities += 1;
                }
                if audio_position == 0 {
                    log.line(format!(
                        "first audio packet frames={} qpc={}",
                        packet.frames, packet.qpc
                    ));
                }
                let position = if packet.qpc > base as u64 {
                    ((packet.qpc - base as u64) as u128 * 48000 / 10_000_000) as u64
                } else {
                    audio_position
                };
                if position > audio_position {
                    let gap = (position - audio_position).min(48000);
                    audio_gap_samples += gap;
                    let silence = vec![0; gap as usize * 4];
                    audio_sample(&writer.sink, audio_stream, &silence, audio_position)?;
                    audio_position += gap;
                }
                let trim = audio_position
                    .saturating_sub(position)
                    .min(packet.frames as u64) as usize;
                if trim < packet.frames as usize {
                    audio_sample(
                        &writer.sink,
                        audio_stream,
                        &packet.bytes[trim * 4..],
                        audio_position,
                    )?;
                    audio_position += (packet.frames as usize - trim) as u64;
                    if audio_position < 2000 {
                        log.line("audio sample written");
                    }
                }
            }
            if stopping {
                break;
            }
            if Instant::now() >= deadline {
                // Timestamp before GPU work and keep the original cadence.
                // Missed slots are skipped below instead of emitted in a burst.
                let sample_time = ticks()? - base;
                if let Some(texture) = &latest {
                    let video_started = Instant::now();
                    let encoded_texture = converter.convert(texture)?;
                    let buffer = MFCreateDXGISurfaceBuffer(
                        &ID3D11Texture2D::IID,
                        &encoded_texture,
                        0,
                        false,
                    )?;
                    buffer.SetCurrentLength(buffer.GetMaxLength()?)?;
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&buffer)?;
                    sample.SetSampleTime(sample_time)?;
                    sample.SetSampleDuration(10_000_000 / 60)?;
                    if count == 0 {
                        log.line("writing first video sample");
                    }
                    writer.sink.WriteSample(video_stream, &sample)?;
                    max_video_us = max_video_us.max(video_started.elapsed().as_micros());
                    if count == 0 {
                        log.line("first video sample written");
                    }
                    count += 1;
                    if !started_feedback && state.started() {
                        crate::media::sound("start");
                        started_feedback = true;
                        log.line(
                            "START_FEEDBACK: first video sample accepted; audio capture running",
                        );
                    }
                } else if start.elapsed() > Duration::from_secs(5) {
                    return Err("5 秒内未收到窗口画面".into());
                }
                deadline += Duration::from_micros(16_667);
                let finished_at = Instant::now();
                while deadline <= finished_at {
                    deadline += Duration::from_micros(16_667);
                }
            }
            if last_report.elapsed() > Duration::from_secs(2) {
                log.line(format!("elapsed={:.2}s captured={captured} encoded={count} audio_frames={audio_position} resizes={resize_count} pool_rebuilds={pool_rebuilds} input_allocations={input_allocations} queued_source_discarded={discarded_queued_frames} audio_queue_peak={} audio_discontinuities={audio_discontinuities} audio_gap_samples={audio_gap_samples} max_video_us={max_video_us} max_pool_us={max_pool_us}",start.elapsed().as_secs_f64(),audio.queue_peak()));
                last_report = Instant::now();
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        state.set("正在保存录像…");
        if count == 0 || !started_feedback {
            cancelled = true;
            state.set("已取消录制");
            return Ok(());
        }
        writer.finish()?;
        log.line(format!("FINALIZED elapsed={:.3}s captured={captured} encoded={count} audio_frames={audio_position} resizes={resize_count} pool_rebuilds={pool_rebuilds} input_allocations={input_allocations} queued_source_discarded={discarded_queued_frames} audio_queue_peak={} audio_discontinuities={audio_discontinuities} audio_gap_samples={audio_gap_samples} max_video_us={max_video_us} max_pool_us={max_pool_us}",start.elapsed().as_secs_f64(),audio.queue_peak()));

        state.set(format!("已保存：{}", path.display()));
        Ok(())
    })();
    if started_feedback && !stopped_feedback {
        crate::media::sound("stop");
    }
    if cancelled && path.exists() {
        if let Err(e) = std::fs::remove_file(&path) {
            log.line(format!("Cannot remove cancelled empty clip: {e}"));
        }
    }
    if let Err(error) = &result {
        log.line(format!("ERROR: {error}"));
    }
    result
}

fn browser_pid(window: &Window) -> AnyResult<u32> {
    let game = window
        .app_handle()
        .get_webview("game")
        .ok_or("game view missing")?;
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    game.with_webview(move |platform| {
        let result = (|| unsafe {
            let mut pid = 0;
            platform
                .controller()
                .CoreWebView2()?
                .BrowserProcessId(&mut pid)?;
            Ok::<u32, windows::core::Error>(pid)
        })();
        let _ = tx.send(result.map_err(|e| e.to_string()));
    })?;
    rx.recv_timeout(Duration::from_secs(3))?
        .map_err(|e| e.into())
}

fn capture_capacity(size: i32) -> i32 {
    size.max(2).saturating_add(255) / 256 * 256
}

/// WGC uses the visible DWM bounds. Crop to the game's client viewport, with
/// native queries only so the capture thread never waits on the UI resize loop.
unsafe fn capture_region(
    window: &Window,
    hwnd: HWND,
    width: u32,
    height: u32,
) -> (u32, u32, u32, u32) {
    use windows::Win32::{
        Foundation::{POINT, RECT},
        Graphics::{
            Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS},
            Gdi::ClientToScreen,
        },
        UI::WindowsAndMessaging::GetClientRect,
    };
    let mut rect = RECT::default();
    let mut bounds = RECT::default();
    let mut origin = POINT::default();
    let top = crate::window_geometry::title_pixels(window);
    if GetClientRect(hwnd, &mut rect).is_err()
        || !ClientToScreen(hwnd, &mut origin).as_bool()
        || DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut bounds as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
        .is_err()
    {
        return (
            0,
            top.min(height - 2),
            width & !1,
            height.saturating_sub(top).max(2) & !1,
        );
    }
    let x = (origin.x - bounds.left).max(0) as u32;
    let y = (origin.y - bounds.top).max(0) as u32 + top;
    let x = x.min(width.saturating_sub(2));
    let y = y.min(height.saturating_sub(2));
    (
        x,
        y,
        (rect.right.max(2) as u32).min(width - x) & !1,
        (rect.bottom.max(2) as u32)
            .saturating_sub(top)
            .max(2)
            .min(height - y)
            & !1,
    )
}
