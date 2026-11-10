//! One warm PCM stream; preloaded clips are mixed into a continuously submitted ring.
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, OnceLock,
    },
    time::{Duration, Instant},
};
use windows::Win32::Media::{Audio::*, MM_WOM_DONE};
const RATE: usize = 48000;
const FRAMES: usize = 480;
const BLOCKS: usize = 4;
static VOICES: AtomicUsize = AtomicUsize::new(0);
static ENGINE: OnceLock<Option<mpsc::SyncSender<Cue>>> = OnceLock::new();
struct Slot;
impl Drop for Slot {
    fn drop(&mut self) {
        VOICES.fetch_sub(1, Ordering::AcqRel);
    }
}
struct Cue {
    kind: usize,
    requested: Instant,
    _slot: Slot,
}
pub fn pending() -> usize {
    VOICES.load(Ordering::Acquire)
}
pub fn warm() {
    let _ = engine();
}
fn engine() -> Option<&'static mpsc::SyncSender<Cue>> {
    ENGINE
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel(32);
            match std::thread::Builder::new()
                .name("feedback-mixer".into())
                .spawn(move || run(rx))
            {
                Ok(_) => Some(tx),
                Err(e) => {
                    eprintln!("Cannot start feedback mixer: {e}");
                    None
                }
            }
        })
        .as_ref()
}
pub fn play(kind: &str) {
    let Some(tx) = engine() else {
        return;
    };
    if VOICES
        .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |n| {
            (n < 32).then_some(n + 1)
        })
        .is_err()
    {
        eprintln!("Feedback mixer is full");
        return;
    }
    let cue = Cue {
        kind: match kind {
            "start" => 0,
            "stop" => 1,
            _ => 2,
        },
        requested: Instant::now(),
        _slot: Slot,
    };
    if tx.try_send(cue).is_err() {
        eprintln!("Feedback mixer is unavailable");
    }
}
fn wav(bytes: &'static [u8]) -> Result<(WAVEFORMATEX, &'static [u8]), String> {
    if bytes.get(..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WAVE") {
        return Err("Invalid WAV".into());
    }
    let mut format = None;
    let mut samples = None;
    let mut offset = 12;
    while offset + 8 <= bytes.len() {
        let len = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let data = bytes
            .get(offset + 8..offset + 8 + len)
            .ok_or("Invalid WAV chunk")?;
        match &bytes[offset..offset + 4] {
            b"fmt " if len >= 16 => {
                let u16_at = |i| u16::from_le_bytes(data[i..i + 2].try_into().unwrap());
                let u32_at = |i| u32::from_le_bytes(data[i..i + 4].try_into().unwrap());
                format = Some(WAVEFORMATEX {
                    wFormatTag: u16_at(0),
                    nChannels: u16_at(2),
                    nSamplesPerSec: u32_at(4),
                    nAvgBytesPerSec: u32_at(8),
                    nBlockAlign: u16_at(12),
                    wBitsPerSample: u16_at(14),
                    cbSize: 0,
                });
            }
            b"data" => samples = Some(data),
            _ => {}
        }
        offset = offset + 8 + len + (len & 1);
    }
    let f = format.ok_or("Missing WAV format")?;
    let data = samples.ok_or("Missing WAV samples")?;
    if f.wFormatTag != 1 || f.wBitsPerSample != 16 || f.nBlockAlign == 0 || data.is_empty() {
        return Err("Unsupported WAV format".into());
    }
    Ok((f, data))
}

fn decode(bytes: &'static [u8]) -> Result<Vec<i16>, String> {
    let (format, data) = wav(bytes)?;
    let channels = format.nChannels as usize;
    let rate = format.nSamplesPerSec as usize;
    if !(1..=2).contains(&channels) || rate == 0 {
        return Err("Unsupported PCM channels/rate".into());
    }
    let input: Vec<i16> = data
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let frames = input.len() / channels;
    let count = frames * RATE / rate;
    let mut output = Vec::with_capacity(count * 2);
    for i in 0..count {
        let position = i as f64 * rate as f64 / RATE as f64;
        let first = (position as usize).min(frames - 1);
        let second = (first + 1).min(frames - 1);
        let fraction = position - first as f64;
        for c in 0..2 {
            let channel = c.min(channels - 1);
            let a = input[first * channels + channel] as f64;
            let b = input[second * channels + channel] as f64;
            output.push((a + (b - a) * fraction).round() as i16);
        }
    }
    Ok(output)
}
unsafe extern "system" fn completed(_: HWAVEOUT, message: u32, _: usize, header: usize, _: usize) {
    if message == MM_WOM_DONE && header != 0 {
        let flag = (*(header as *const WAVEHDR)).dwUser as *const AtomicBool;
        if !flag.is_null() {
            (*flag).store(true, Ordering::Release);
        }
    }
}
struct Block {
    samples: Box<[i16]>,
    header: Option<Box<WAVEHDR>>,
    done: Option<Box<AtomicBool>>,
    retired: Vec<Cue>,
    prepared: bool,
}
struct Device {
    handle: HWAVEOUT,
    blocks: Vec<Block>,
}
impl Drop for Device {
    fn drop(&mut self) {
        unsafe {
            let _ = waveOutReset(self.handle);
            for b in &mut self.blocks {
                if b.prepared
                    && waveOutUnprepareHeader(
                        self.handle,
                        &mut **b.header.as_mut().unwrap(),
                        std::mem::size_of::<WAVEHDR>() as u32,
                    ) != 0
                {
                    // Broken drivers may still retain these buffers; keep them alive.
                    if let Some(v) = b.header.take() {
                        std::mem::forget(v);
                    }
                    if let Some(v) = b.done.take() {
                        std::mem::forget(v);
                    }
                    std::mem::forget(std::mem::replace(
                        &mut b.samples,
                        Vec::new().into_boxed_slice(),
                    ));
                }
            }
            let _ = waveOutClose(self.handle);
        }
    }
}
impl Device {
    unsafe fn open() -> Result<Self, String> {
        let format = WAVEFORMATEX {
            wFormatTag: 1,
            nChannels: 2,
            nSamplesPerSec: RATE as u32,
            nAvgBytesPerSec: RATE as u32 * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut handle = HWAVEOUT::default();
        let result = waveOutOpen(
            Some(&mut handle),
            WAVE_MAPPER,
            &format,
            Some(completed as *const () as usize),
            None,
            CALLBACK_FUNCTION,
        );
        if result != 0 {
            return Err(format!("waveOutOpen {result}"));
        }
        let mut device = Self {
            handle,
            blocks: Vec::with_capacity(BLOCKS),
        };
        for _ in 0..BLOCKS {
            let mut samples = vec![0i16; FRAMES * 2].into_boxed_slice();
            let done = Box::new(AtomicBool::new(true));
            let header = Box::new(WAVEHDR {
                lpData: windows::core::PSTR(samples.as_mut_ptr().cast()),
                dwBufferLength: (samples.len() * 2) as u32,
                dwUser: (&*done as *const AtomicBool) as usize,
                ..Default::default()
            });
            device.blocks.push(Block {
                samples,
                header: Some(header),
                done: Some(done),
                retired: Vec::with_capacity(32),
                prepared: false,
            });
            let block = device.blocks.last_mut().unwrap();
            let result = waveOutPrepareHeader(
                handle,
                &mut **block.header.as_mut().unwrap(),
                std::mem::size_of::<WAVEHDR>() as u32,
            );
            if result != 0 {
                return Err(format!("waveOutPrepareHeader {result}"));
            }
            block.prepared = true;
        }
        eprintln!("feedback: warm persistent stream opened, 48kHz stereo, 4 x 10ms buffers");
        Ok(device)
    }
}
struct Playing {
    cue: Cue,
    position: usize,
}
fn mix(
    output: &mut [i16],
    voices: &mut Vec<Playing>,
    assets: &[Vec<i16>; 3],
    retired: &mut Vec<Cue>,
) {
    let mut accum = [0i32; FRAMES * 2];
    for voice in voices.iter_mut() {
        let samples = &assets[voice.cue.kind];
        let count = (samples.len() - voice.position).min(output.len());
        for i in 0..count {
            accum[i] += samples[voice.position + i] as i32;
        }
        voice.position += count;
    }
    for (out, value) in output.iter_mut().zip(accum) {
        *out = value.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
    }
    let mut i = 0;
    while i < voices.len() {
        if voices[i].position >= assets[voices[i].cue.kind].len() {
            retired.push(voices.remove(i).cue);
        } else {
            i += 1;
        }
    }
}
unsafe fn pump(
    device: &mut Device,
    rx: &mpsc::Receiver<Cue>,
    assets: &[Vec<i16>; 3],
) -> Result<(), String> {
    let mut voices = Vec::with_capacity(32);
    let mut queued = 0usize;
    let mut completed_blocks = 0usize;
    let mut progress = Instant::now();
    loop {
        while let Ok(cue) = rx.try_recv() {
            if cue.requested.elapsed() < Duration::from_secs(2) {
                voices.push(Playing { cue, position: 0 });
            } else {
                eprintln!("feedback: expired cue after device recovery");
            }
        }
        // Recycle strictly in submission order, preserving both PCM and cue ordering.
        let index = completed_blocks % BLOCKS;
        let block = &mut device.blocks[index];
        if block.done.as_ref().unwrap().swap(false, Ordering::AcqRel) {
            block.retired.clear();
            mix(&mut block.samples, &mut voices, assets, &mut block.retired);
            let result = waveOutWrite(
                device.handle,
                &mut **block.header.as_mut().unwrap(),
                std::mem::size_of::<WAVEHDR>() as u32,
            );
            if result != 0 {
                return Err(format!("waveOutWrite {result}"));
            }
            queued += 1;
            completed_blocks += 1;
            progress = Instant::now();
            // Fill the initial ring immediately; thereafter idle until a block completes.
            if queued < BLOCKS {
                continue;
            }
        } else if progress.elapsed() > Duration::from_secs(2) {
            return Err("Audio device stopped completing buffers".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn run(rx: mpsc::Receiver<Cue>) {
    let assets = match (|| -> Result<[Vec<i16>; 3], String> {
        Ok([
            decode(include_bytes!("../sounds/recording_start.wav"))?,
            decode(include_bytes!("../sounds/recording_stop.wav"))?,
            decode(include_bytes!("../sounds/camera1.wav"))?,
        ])
    })() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Feedback preload failed: {e}");
            return;
        }
    };
    loop {
        let result =
            unsafe { Device::open().and_then(|mut device| pump(&mut device, &rx, &assets)) };
        if let Err(e) = result {
            eprintln!("Feedback stream failed, retrying: {e}");
        }
        while rx.try_recv().is_ok() {} // Drop queued slots on device failure; never hang exit.
        std::thread::sleep(Duration::from_millis(500));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mixer_preserves_first_sample_and_overlapping_voices() {
        let assets = [
            vec![1000; FRAMES * 2],
            vec![2000; FRAMES * 2],
            vec![0; FRAMES * 2],
        ];
        VOICES.fetch_add(2, Ordering::AcqRel);
        let mut voices = vec![
            Playing {
                cue: Cue {
                    kind: 0,
                    requested: Instant::now(),
                    _slot: Slot,
                },
                position: 0,
            },
            Playing {
                cue: Cue {
                    kind: 1,
                    requested: Instant::now(),
                    _slot: Slot,
                },
                position: 0,
            },
        ];
        let mut output = [-1i16; FRAMES * 2];
        let mut retired = Vec::new();
        mix(&mut output, &mut voices, &assets, &mut retired);
        assert!(output.iter().all(|v| *v == 3000));
        assert!(voices.is_empty());
        assert_eq!(retired.len(), 2);
        retired.clear();
        mix(&mut output, &mut voices, &assets, &mut retired);
        assert!(output.iter().all(|v| *v == 0));
    }
    #[test]
    fn preload_keeps_pcm_duration() {
        let pcm = decode(include_bytes!("../sounds/camera1.wav")).unwrap();
        assert_eq!(pcm.len() / 2, 4490 * RATE / 22050);
    }
    #[test]
    fn embedded_waves_are_valid_pcm() {
        for b in [
            include_bytes!("../sounds/recording_start.wav").as_slice(),
            include_bytes!("../sounds/recording_stop.wav").as_slice(),
            include_bytes!("../sounds/camera1.wav").as_slice(),
        ] {
            let (f, d) = wav(b).unwrap();
            let alignment = f.nBlockAlign;
            assert_eq!(d.len() % alignment as usize, 0);
        }
        assert!(wav(b"bad").is_err());
    }
}
