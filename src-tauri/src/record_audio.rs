//! Process-tree loopback only: no desktop audio or microphone fallback.
use std::{mem::ManuallyDrop, sync::mpsc, time::Duration};
use windows::{
    core::{implement, Interface, Ref, Result, HRESULT},
    Win32::{
        Media::Audio::*,
        System::{
            Com::{
                StructuredStorage::{
                    PROPVARIANT, PROPVARIANT_0, PROPVARIANT_0_0, PROPVARIANT_0_0_0,
                },
                BLOB,
            },
            Variant::VT_BLOB,
        },
    },
};
#[implement(IActivateAudioInterfaceCompletionHandler)]
struct Completion(mpsc::SyncSender<std::result::Result<IAudioClient, String>>);
impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(
        &self,
        operation: Ref<IActivateAudioInterfaceAsyncOperation>,
    ) -> Result<()> {
        let result = (|| unsafe {
            let mut status = HRESULT(0);
            let mut object = None;
            operation
                .as_ref()
                .ok_or_else(windows::core::Error::from_thread)?
                .GetActivateResult(&mut status, &mut object)?;
            status.ok()?;
            object
                .ok_or_else(windows::core::Error::from_thread)?
                .cast::<IAudioClient>()
        })();
        let _ = self.0.send(result.map_err(|e| e.to_string()));
        Ok(())
    }
}
pub struct Audio {
    capture: IAudioCaptureClient,
    client: IAudioClient,
    _activation: IActivateAudioInterfaceAsyncOperation,
    _handler: IActivateAudioInterfaceCompletionHandler,
    _params: Box<AUDIOCLIENT_ACTIVATION_PARAMS>,
    _variant: Box<ManuallyDrop<PROPVARIANT>>,
}
pub struct Packet {
    pub bytes: Vec<u8>,
    pub frames: u32,
    pub qpc: u64,
    pub discontinuity: bool,
}
impl Audio {
    pub unsafe fn new(pid: u32) -> std::result::Result<Self, String> {
        let mut params = Box::new(AUDIOCLIENT_ACTIVATION_PARAMS {
            ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
            Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
                ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                    TargetProcessId: pid,
                    ProcessLoopbackMode: PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE,
                },
            },
        });
        let blob = BLOB {
            cbSize: std::mem::size_of_val(params.as_ref()) as u32,
            pBlobData: params.as_mut() as *mut _ as *mut u8,
        };
        // PROPVARIANT::drop calls PropVariantClear, but this BLOB borrows our
        // Rust-owned params. Prevent the variant from freeing that allocation.
        let variant = Box::new(ManuallyDrop::new(PROPVARIANT {
            Anonymous: PROPVARIANT_0 {
                Anonymous: ManuallyDrop::new(PROPVARIANT_0_0 {
                    vt: VT_BLOB,
                    Anonymous: PROPVARIANT_0_0_0 { blob },
                    ..Default::default()
                }),
            },
        }));
        let (tx, rx) = mpsc::sync_channel(1);
        let handler: IActivateAudioInterfaceCompletionHandler = Completion(tx).into();
        eprintln!("audio: activating process loopback");
        let _operation = ActivateAudioInterfaceAsync(
            VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
            &IAudioClient::IID,
            Some(&**variant),
            &handler,
        )
        .map_err(|e| format!("进程音频初始化：{e}"))?;
        eprintln!("audio: activation requested");
        let client = rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())??;
        eprintln!("audio: activation callback received");
        let format = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 2,
            nSamplesPerSec: 48000,
            nAvgBytesPerSec: 192000,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK
                    | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                    | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                1_000_000,
                0,
                &format,
                None,
            )
            .map_err(|e| format!("进程音频格式：{e}"))?;
        eprintln!("audio: client initialized");
        let capture = client
            .GetService::<IAudioCaptureClient>()
            .map_err(|e| e.to_string())?;
        eprintln!("audio: capture client obtained");
        Ok(Self {
            client,
            capture,
            _activation: _operation,
            _handler: handler,
            _params: params,
            _variant: variant,
        })
    }
    pub unsafe fn start(&self) -> Result<()> {
        self.client.Start()
    }
    pub unsafe fn read(&self) -> Result<Option<Packet>> {
        if self.capture.GetNextPacketSize()? == 0 {
            return Ok(None);
        }
        let (mut ptr, mut frames, mut flags, mut qpc) = (std::ptr::null_mut(), 0, 0, 0);
        self.capture
            .GetBuffer(&mut ptr, &mut frames, &mut flags, None, Some(&mut qpc))?;
        let length = frames as usize * 4;
        let bytes = if flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0 || ptr.is_null() {
            vec![0; length]
        } else {
            std::slice::from_raw_parts(ptr, length).to_vec()
        };
        self.capture.ReleaseBuffer(frames)?;
        if flags & AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR.0 as u32 != 0 {
            qpc = 0;
        }
        Ok(Some(Packet {
            bytes,
            frames,
            qpc,
            discontinuity: flags & AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY.0 as u32 != 0,
        }))
    }
}
impl Drop for Audio {
    fn drop(&mut self) {
        unsafe {
            eprintln!("audio: stopping");
            let _ = self.client.Stop();
            eprintln!("audio: releasing services");
        }
    }
}

// Capture stays on its own MTA thread. The video/MF writer thread cannot block
// WASAPI servicing. A bounded queue absorbs short GPU stalls, never silently
// dropping audio; sustained overload is reported as an explicit recording error.
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            RoUninitialize();
        }
    }
}
pub struct AudioCapture {
    rx: mpsc::Receiver<Packet>,
    begin: mpsc::SyncSender<()>,
    stop: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<String>>>,
    queued: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    join: Option<JoinHandle<()>>,
}
impl AudioCapture {
    pub fn new(pid: u32) -> std::result::Result<Self, String> {
        let (tx, rx) = mpsc::sync_channel(1024);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (begin, start_rx) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let failure = Arc::new(Mutex::new(None));
        let thread_failure = failure.clone();
        let queued = Arc::new(AtomicUsize::new(0));
        let thread_queued = queued.clone();
        let peak = Arc::new(AtomicUsize::new(0));
        let thread_peak = peak.clone();
        let join = std::thread::Builder::new()
            .name("wasapi-capture".into())
            .spawn(move || unsafe {
                if let Err(e) = RoInitialize(RO_INIT_MULTITHREADED) {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
                let _apartment = Apartment;
                let audio = match Audio::new(pid) {
                    Ok(a) => a,
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                if ready_tx.send(Ok(())).is_err() {
                    return;
                }
                loop {
                    if thread_stop.load(Ordering::Relaxed) {
                        return;
                    }
                    match start_rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(()) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(_) => return,
                    }
                }
                if thread_stop.load(Ordering::Relaxed) {
                    return;
                }
                if let Err(e) = audio.start() {
                    *thread_failure.lock().unwrap() = Some(e.to_string());
                    return;
                }
                while !thread_stop.load(Ordering::Relaxed) {
                    loop {
                        let packet = match audio.read() {
                            Ok(Some(p)) => p,
                            Ok(None) => break,
                            Err(e) => {
                                *thread_failure.lock().unwrap() = Some(e.to_string());
                                return;
                            }
                        };
                        let count = thread_queued.fetch_add(1, Ordering::SeqCst) + 1;
                        match tx.try_send(packet) {
                            Ok(()) => {
                                thread_peak.fetch_max(count, Ordering::Relaxed);
                            }
                            Err(mpsc::TrySendError::Full(_)) => {
                                thread_queued.fetch_sub(1, Ordering::SeqCst);
                                *thread_failure.lock().unwrap() = Some(
                                    "音频队列已满，录制管线持续阻塞；已停止以避免静默丢音".into(),
                                );
                                return;
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => {
                                thread_queued.fetch_sub(1, Ordering::SeqCst);
                                return;
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            })
            .map_err(|e| e.to_string())?;
        let capture = Self {
            rx,
            begin,
            stop,
            failure,
            queued,
            peak,
            join: Some(join),
        };
        match ready_rx.recv_timeout(Duration::from_secs(6)) {
            Ok(Ok(())) => Ok(capture),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(format!("音频采集线程初始化失败：{e}")),
        }
    }
    pub fn start(&self) -> std::result::Result<(), String> {
        self.begin.try_send(()).map_err(|e| e.to_string())
    }
    pub fn read(&self) -> std::result::Result<Option<Packet>, String> {
        if let Some(e) = self.failure.lock().unwrap().take() {
            return Err(e);
        }
        match self.rx.try_recv() {
            Ok(packet) => {
                self.queued.fetch_sub(1, Ordering::SeqCst);
                Ok(Some(packet))
            }
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) if self.stop.load(Ordering::Relaxed) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err("音频采集线程意外结束".into()),
        }
    }
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.begin.try_send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
    pub fn queue_peak(&self) -> usize {
        self.peak.load(Ordering::Relaxed)
    }
}
impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}
