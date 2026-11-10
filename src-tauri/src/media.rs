//! Capture only the official game WebView. Native sounds are outside its audio process tree.
use std::{
    io::Write,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    sync::{mpsc, Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{Manager, Window};
use windows::Win32::{System::Com::*, UI::Shell::*};
pub fn sound(kind: &str) {
    crate::feedback::play(kind);
}
// Native WebView key arrival is observable; physical key-down time is not.
static WEB_KEY: Mutex<Option<Instant>> = Mutex::new(None);
pub fn observe_screenshot_key() {
    *WEB_KEY.lock().unwrap() = Some(Instant::now());
}
pub fn clear_screenshot_key() {
    *WEB_KEY.lock().unwrap() = None;
}

fn ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.
}
fn timing_log(window: &Window, value: &serde_json::Value) {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let dir = window
            .app_handle()
            .path()
            .app_local_data_dir()?
            .join("Screenshots");
        std::fs::create_dir_all(&dir)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("timing.jsonl"))?;
        writeln!(f, "{value}")?;
        Ok(())
    })();
    eprintln!("screenshot timing: {value}");
    if let Err(e) = result {
        eprintln!("Cannot write screenshot timing: {e}");
    }
}
// Count actual file writes underneath the JPEG output buffer.
struct TimedFile {
    file: std::fs::File,
    calls: u64,
    bytes: u64,
    time: Duration,
}
impl Write for TimedFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let start = Instant::now();
        let result = self.file.write(bytes);
        self.time += start.elapsed();
        self.calls += 1;
        if let Ok(n) = &result {
            self.bytes += *n as u64;
        }
        result
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let start = Instant::now();
        let result = self.file.flush();
        self.time += start.elapsed();
        result
    }
}
const MAX_SHOTS: usize = 16;
const MAX_QUEUED_BYTES: usize = 128 * 1024 * 1024;
#[derive(Default)]
struct QueueState {
    pending: AtomicUsize,
    bytes: AtomicUsize,
    closing: AtomicBool,
    admission: Mutex<()>,
}
struct Ticket {
    state: Arc<QueueState>,
    bytes: usize,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        self.state.bytes.fetch_sub(self.bytes, Ordering::AcqRel);
        self.state.pending.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Shot {
    window: Window,
    start: Instant,
    config: crate::preferences::Settings,
    timing: serde_json::Value,
    path: std::path::PathBuf,
    bytes: Vec<u8>,
    _ticket: Ticket,
}
struct Flight {
    ticket: Mutex<Option<Ticket>>,
    window: Window,
    start: Instant,
    id: u64,
}
struct Service {
    state: Arc<QueueState>,
    writer: mpsc::SyncSender<Shot>,
    flights: Arc<Mutex<Vec<Weak<Flight>>>>,
}
static SHOTS: OnceLock<Service> = OnceLock::new();
static NEXT_SHOT: AtomicU64 = AtomicU64::new(1);
fn service() -> &'static Service {
    SHOTS.get_or_init(|| {
        let state = Arc::new(QueueState::default());
        let flights=Arc::new(Mutex::new(Vec::<Weak<Flight>>::new()));
        let watch=flights.clone();
        std::thread::Builder::new().name("screenshot-timeouts".into()).spawn(move||loop{
            std::thread::sleep(Duration::from_millis(100));
            let mut expired=Vec::new();
            watch.lock().unwrap().retain(|weak|{
                let Some(flight)=weak.upgrade() else{return false;};
                if flight.start.elapsed()>Duration::from_secs(8){
                    if let Some(ticket)=flight.ticket.lock().unwrap().take(){drop(ticket);expired.push(flight.clone());}
                    false
                }else{true}
            });
            for flight in expired{notify(&flight.window,"截图超时，请重试");timing_log(&flight.window,&serde_json::json!({"event":"capture_timeout","shot_id":flight.id,"total_ms":ms(flight.start),"success":false}));}
        }).expect("Cannot start screenshot timeout monitor");
        let (tx, rx) = mpsc::sync_channel::<Shot>(MAX_SHOTS);
        std::thread::Builder::new()
            .name("screenshot-save".into())
            .spawn(move || {
                while let Ok(mut shot) = rx.recv() {
                    shot.timing["encoding_started_ms"] = ms(shot.start).into();
                    let result = save_shot(&mut shot);
                    shot.timing["total_ms"] = ms(shot.start).into();
                    shot.timing["success"] = result.is_ok().into();
                    if let Err(e) = result {
                        shot.timing["error"] = e.clone().into();
                        notify(&shot.window, &format!("截图保存失败：{e}"));
                    }
                    timing_log(&shot.window, &shot.timing);
                }
            })
            .expect("Cannot start screenshot writer");
        Service { state, writer: tx, flights }
    })
}
pub fn pending_screenshots() -> usize {
    SHOTS
        .get()
        .map(|s| s.state.pending.load(Ordering::Acquire))
        .unwrap_or(0)
}
pub fn close_screenshots() {
    if let Some(s) = SHOTS.get() {
        let _admission = s.state.admission.lock().unwrap();
        s.state.closing.store(true, Ordering::Release);
    }
}
pub fn screenshot(window: Window) {
    screenshot_from(window, "button");
}
pub fn screenshot_from(window: Window, source: &'static str) {
    let start = Instant::now();
    let service = service();
    let admission = service.state.admission.lock().unwrap();
    if service.state.closing.load(Ordering::Acquire)
        || window
            .state::<crate::ExitApproved>()
            .0
            .load(Ordering::Acquire)
    {
        return;
    }
    if service
        .state
        .pending
        .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |n| {
            (n < MAX_SHOTS).then_some(n + 1)
        })
        .is_err()
    {
        notify(&window, "截图队列已满，请稍后重试");
        return;
    }
    let ticket = Ticket {
        state: service.state.clone(),
        bytes: 0,
    };
    drop(admission);
    let observed = if source == "keyboard" {
        WEB_KEY
            .lock()
            .unwrap()
            .take()
            .filter(|t| t.elapsed() < Duration::from_millis(100))
    } else {
        None
    };
    let config = window.state::<crate::preferences::Preferences>().settings();
    let Some(game) = window.app_handle().get_webview("game") else {
        notify(&window, "游戏视图不可用");
        return;
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let id = NEXT_SHOT.fetch_add(1, Ordering::Relaxed);
    let flight = Arc::new(Flight {
        ticket: Mutex::new(Some(ticket)),
        window: window.clone(),
        start,
        id,
    });
    service
        .flights
        .lock()
        .unwrap()
        .push(Arc::downgrade(&flight));
    let path = config
        .folder
        .join(format!("StarRail-{stamp}-{}-{id}", std::process::id()));
    let mut timing = serde_json::json!({"event":"screenshot","version":env!("CARGO_PKG_VERSION"),"debug_build":cfg!(debug_assertions),"source":source,"mode":config.screenshot,"epoch_ms":stamp as u64,"shot_id":id,"file_base":path.to_string_lossy(),"web_key_to_dispatch_ms":observed.map(|t|start.duration_since(t).as_secs_f64()*1000.),"webview_queued_ms":ms(start),"pending_at_request":service.state.pending.load(Ordering::Relaxed)});
    let error_window = window.clone();
    let result=game.with_webview(move|platform|unsafe{
        timing["webview_dispatched_ms"]=ms(start).into();
        let error_window=window.clone();
        let result=(||->windows::core::Result<()>{
            let stream=SHCreateMemStream(None).ok_or_else(windows::core::Error::from_thread)?;let output=stream.clone();
            timing["capture_requested_ms"]=ms(start).into();
            let callback=webview2_com::CapturePreviewCompletedHandler::create(Box::new(move|status|{
                let Some(mut ticket)=flight.ticket.lock().unwrap().take()else{return Ok(());};
                timing["capture_callback_ms"]=ms(start).into();
                let captured=(||->Result<Vec<u8>,String>{
                    status.map_err(|e|e.to_string())?;let mut stat=STATSTG::default();output.Stat(&mut stat,STATFLAG_NONAME).map_err(|e|e.to_string())?;
                    let len=usize::try_from(stat.cbSize).map_err(|e|e.to_string())?;
                    if len==0||len>64*1024*1024{return Err("截图尺寸超出限制".into());}
                    ticket.state.bytes.fetch_update(Ordering::AcqRel,Ordering::Relaxed,|n|n.checked_add(len).filter(|v|*v<=MAX_QUEUED_BYTES)).map_err(|_|"截图保存队列内存已满，请稍后重试")?;
                    ticket.bytes=len;
                    output.Seek(0,STREAM_SEEK_SET,None).map_err(|e|e.to_string())?;
                    let mut bytes=vec![0;len];let mut read=0;output.Read(bytes.as_mut_ptr().cast(),len as u32,Some(&mut read)).ok().map_err(|e|e.to_string())?;
                    if read as usize!=len{return Err("截图数据不完整".into());}Ok(bytes)
                })();
                match captured{
                    Ok(bytes)=>{
                        timing["stream_ready_ms"]=ms(start).into();timing["capture_png_bytes"]=bytes.len().into();
                        // Pixels have been captured; all subsequent work is independent of capture.
                        sound("camera");timing["feedback_requested_ms"]=ms(start).into();
                        let shot=Shot{window:window.clone(),start,config,timing,path,bytes,_ticket:ticket};
                        if service.writer.try_send(shot).is_err(){notify(&window,"截图保存队列不可用");}
                    },
                    Err(e)=>{timing["error"]=e.clone().into();timing["success"]=false.into();timing["total_ms"]=ms(start).into();timing_log(&window,&timing);notify(&window,&format!("截图失败：{e}"));}
                }Ok(())
            }));
            platform.controller().CoreWebView2()?.CapturePreview(webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,&stream,&callback)
        })();if let Err(e)=result{notify(&error_window,&format!("无法获取截图：{e}"));}
    });
    if let Err(e) = result {
        notify(&error_window, &format!("无法请求截图：{e}"));
    }
}
fn save_shot(shot: &mut Shot) -> Result<(), String> {
    let Shot {
        window,
        start,
        config,
        timing,
        path,
        bytes,
        ..
    } = shot;
    let phase = Instant::now();
    std::fs::create_dir_all(&config.folder).map_err(|e| e.to_string())?;
    timing["directory_ms"] = ms(phase).into();
    let mut saved = Vec::new();
    if config.screenshot != "compressed" {
        let p = path.with_extension("png");
        let phase = Instant::now();
        std::fs::write(&p, bytes.as_slice()).map_err(|e| e.to_string())?;
        timing["png_write_ms"] = ms(phase).into();
        saved.push(p);
    }
    if config.screenshot != "original" {
        let phase = Instant::now();
        let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .map_err(|e| e.to_string())?
            .to_rgb8();
        timing["png_decode_rgb_ms"] = ms(phase).into();
        timing["width"] = image.width().into();
        timing["height"] = image.height().into();
        let p = path.with_extension("jpg");
        let f = std::fs::File::create(&p).map_err(|e| e.to_string())?;
        let mut output = TimedFile {
            file: f,
            calls: 0,
            bytes: 0,
            time: Duration::ZERO,
        };
        let phase = Instant::now();
        {
            let mut buffered = std::io::BufWriter::with_capacity(256 * 1024, &mut output);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buffered, 90)
                .encode(
                    image.as_raw(),
                    image.width(),
                    image.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| e.to_string())?;
            buffered.flush().map_err(|e| e.to_string())?;
        }
        timing["jpeg_buffer_bytes"] = (256 * 1024).into();
        timing["jpeg_encode_write_ms"] = ms(phase).into();
        timing["jpeg_io_ms"] = (output.time.as_secs_f64() * 1000.).into();
        timing["jpeg_write_calls"] = output.calls.into();
        saved.push(p);
    }
    timing["files_written_ms"] = ms(*start).into();
    notify(window, &format!("截图已保存：{}", saved[0].display()));
    Ok(())
}
pub fn notify(window: &Window, text: &str) {
    if let Some(view) = window.app_handle().get_webview("main") {
        let _ = view.eval(format!(
            "window.showNotice?.({})",
            serde_json::to_string(text).unwrap()
        ));
    }
}
pub fn choose_folder(window: &Window) -> Result<Option<String>, String> {
    unsafe {
        // Called on the UI STA, with the application window as the dialog owner.
        let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| e.to_string())?;
        dialog
            .SetOptions(
                dialog.GetOptions().map_err(|e| e.to_string())?
                    | FOS_PICKFOLDERS
                    | FOS_FORCEFILESYSTEM
                    | FOS_NOCHANGEDIR,
            )
            .map_err(|e| e.to_string())?;
        dialog
            .SetTitle(&windows::core::HSTRING::from("选择截图和录像保存位置"))
            .map_err(|e| e.to_string())?;
        if let Err(e) = dialog.Show(Some(windows::Win32::Foundation::HWND(
            window.hwnd().map_err(|e| e.to_string())?.0,
        ))) {
            if e.code().0 == 0x800704c7u32 as i32 {
                return Ok(None);
            }
            return Err(e.to_string());
        }
        let item = dialog.GetResult().map_err(|e| e.to_string())?;
        let ptr = item
            .GetDisplayName(SIGDN_FILESYSPATH)
            .map_err(|e| e.to_string())?;
        let value = ptr.to_string().map_err(|e| e.to_string());
        CoTaskMemFree(Some(ptr.0.cast()));
        value.map(Some)
    }
}
