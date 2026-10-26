//! Microphone capture with AVAudioEngine. The mic is open only while recording.
//!
//! Audio arrives on Core Audio's thread in ~85 ms buffers. Channel 0 is kept as
//! 16-bit samples at the device's own rate; the engine helper resamples to 16 kHz.
//!
//! Building an engine and its input node takes 250 ms or more, so one is kept built
//! and prepared between recordings (`prewarm`). A prepared engine doesn't open the
//! mic; only `start` does. It is rebuilt when the default input device changes.

use std::cell::RefCell;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_avf_audio::{AVAudioEngine, AVAudioPCMBuffer, AVAudioTime};

type TapBlock = RcBlock<dyn Fn(NonNull<AVAudioPCMBuffer>, NonNull<AVAudioTime>)>;

struct Active {
    engine: Retained<AVAudioEngine>,
    device: u32,
    _tap: TapBlock,
    rate: u32,
    started: Instant,
}

/// A built, prepared engine waiting for the next recording, and the input device it uses.
struct Warm {
    engine: Retained<AVAudioEngine>,
    device: u32,
}

static SAMPLES: Mutex<Vec<i16>> = Mutex::new(Vec::new());
/// RMS of the latest buffer (f32 bits), for the recording indicator.
static LEVEL: AtomicU32 = AtomicU32::new(0);
/// When the first buffer with any sound in it arrived. Bluetooth mics send silence
/// while they switch over, so this is when the mic is really listening.
static LIVE_AT: Mutex<Option<Instant>> = Mutex::new(None);

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
    static WARM: RefCell<Option<Warm>> = const { RefCell::new(None) };
}

#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
}

/// The system's default input device (Core Audio id), or 0 if there isn't one.
fn default_input() -> u32 {
    // kAudioObjectSystemObject, kAudioHardwarePropertyDefaultInputDevice ('dIn '),
    // kAudioObjectPropertyScopeGlobal ('glob'), kAudioObjectPropertyElementMain.
    let address = PropertyAddress { selector: 0x6449_6e20, scope: 0x676c_6f62, element: 0 };
    let mut id = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(1, &address, 0, std::ptr::null(), &mut size, (&raw mut id).cast())
    };
    if status == 0 { id } else { 0 }
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectAddPropertyListenerBlock(
        object: u32,
        address: *const PropertyAddress,
        queue: *const c_void,
        listener: *const block2::Block<dyn Fn(u32, *const c_void)>,
    ) -> i32;
}

/// Run `f` on the main thread whenever the default input device changes
/// (AirPods connect, a mic is plugged in), so a matching engine is ready in time.
pub fn watch_default_input(f: fn()) {
    let address = PropertyAddress { selector: 0x6449_6e20, scope: 0x676c_6f62, element: 0 };
    let block = RcBlock::new(move |_: u32, _: *const c_void| crate::util::main_async(f));
    // A null queue runs the block on Core Audio's own thread; it only hops to main.
    let status = unsafe { AudioObjectAddPropertyListenerBlock(1, &address, std::ptr::null(), RcBlock::as_ptr(&block)) };
    if status == 0 {
        // Core Audio copies the block and keeps it for as long as Tamka runs.
        std::mem::forget(block);
    }
}

fn build(device: u32) -> Warm {
    let engine = unsafe { AVAudioEngine::new() };
    unsafe {
        engine.inputNode();
        engine.prepare();
    }
    Warm { engine, device }
}

/// Build and prepare an engine now so the next `start` is quick. Cheap if one is ready.
pub fn prewarm() {
    if is_recording() {
        return;
    }
    let device = default_input();
    WARM.with(|w| {
        let mut w = w.borrow_mut();
        if w.as_ref().is_none_or(|w| w.device != device) {
            let t = Instant::now();
            *w = Some(build(device));
            crate::util::log(format!("audio engine ready in {} ms", t.elapsed().as_millis()));
        }
    });
}

/// When the mic first sent sound, measured from `since`.
pub fn live_after(since: Instant) -> Option<Duration> {
    LIVE_AT.lock().unwrap().map(|t| t.saturating_duration_since(since))
}

pub struct Recording {
    pub samples: Vec<i16>,
    pub rate: u32,
}

impl Recording {
    pub fn seconds(&self) -> f64 {
        self.samples.len() as f64 / self.rate.max(1) as f64
    }
}

pub fn is_recording() -> bool {
    ACTIVE.with(|a| a.borrow().is_some())
}

pub fn elapsed() -> Duration {
    ACTIVE.with(|a| a.borrow().as_ref().map(|a| a.started.elapsed()).unwrap_or_default())
}

/// 0.0–1.0, roughly how loud the last ~85 ms were.
pub fn level() -> f32 {
    (f32::from_bits(LEVEL.load(Ordering::Relaxed)) * 8.0).min(1.0)
}

/// Open the mic and start collecting audio. Returns whether a prepared engine was used.
pub fn start() -> Result<bool, String> {
    if is_recording() {
        return Ok(true);
    }
    let device = default_input();
    let warm = WARM.with(|w| w.borrow_mut().take()).filter(|w| w.device == device);
    let reused = warm.is_some();
    let Warm { engine, device } = warm.unwrap_or_else(|| build(device));
    let input = unsafe { engine.inputNode() };
    let format = unsafe { input.outputFormatForBus(0) };
    let rate = unsafe { format.sampleRate() };
    if rate <= 0.0 || unsafe { format.channelCount() } == 0 {
        return Err("No microphone is available.".into());
    }
    {
        let mut s = SAMPLES.lock().unwrap();
        s.clear();
        s.reserve(rate as usize * 60);
    }
    LEVEL.store(0, Ordering::Relaxed);
    *LIVE_AT.lock().unwrap() = None;

    let tap: TapBlock = RcBlock::new(|buffer: NonNull<AVAudioPCMBuffer>, _when: NonNull<AVAudioTime>| {
        // SAFETY: AVAudioEngine hands us a valid buffer for the duration of the call.
        let buffer = unsafe { buffer.as_ref() };
        let frames = unsafe { buffer.frameLength() } as usize;
        if frames == 0 {
            return;
        }
        let mut sum = 0.0f32;
        let mut out = SAMPLES.lock().unwrap();
        let float = unsafe { buffer.floatChannelData() };
        if !float.is_null() {
            let ch0 = unsafe { std::slice::from_raw_parts((*float).as_ptr(), frames) };
            for &s in ch0 {
                sum += s * s;
                out.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
        } else {
            let int16 = unsafe { buffer.int16ChannelData() };
            if int16.is_null() {
                return;
            }
            let ch0 = unsafe { std::slice::from_raw_parts((*int16).as_ptr(), frames) };
            for &s in ch0 {
                let f = s as f32 / 32768.0;
                sum += f * f;
                out.push(s);
            }
        }
        if sum > 0.0 {
            LIVE_AT.lock().unwrap().get_or_insert_with(Instant::now);
        }
        LEVEL.store((sum / frames as f32).sqrt().to_bits(), Ordering::Relaxed);
    });

    unsafe {
        input.installTapOnBus_bufferSize_format_block(0, 4096, Some(&format), RcBlock::as_ptr(&tap));
        engine.prepare();
    }
    if let Err(e) = unsafe { engine.startAndReturnError() } {
        unsafe { input.removeTapOnBus(0) };
        return Err(e.localizedDescription().to_string());
    }
    ACTIVE.with(|a| {
        *a.borrow_mut() =
            Some(Active { engine, device, _tap: tap, rate: rate.round() as u32, started: Instant::now() })
    });
    Ok(reused)
}

/// Stop and return what was recorded.
pub fn stop() -> Option<Recording> {
    let active = ACTIVE.with(|a| a.borrow_mut().take())?;
    unsafe {
        active.engine.inputNode().removeTapOnBus(0);
        active.engine.stop();
        active.engine.prepare();
    }
    // Keep the engine for next time; `start` checks it still matches the input device.
    WARM.with(|w| *w.borrow_mut() = Some(Warm { engine: active.engine, device: active.device }));
    let samples = std::mem::take(&mut *SAMPLES.lock().unwrap());
    Some(Recording { samples, rate: active.rate })
}

/// Stop and throw the audio away.
pub fn cancel() {
    drop(stop());
}

/// 16-bit mono PCM WAV.
pub fn write_wav(path: &Path, rec: &Recording) -> std::io::Result<()> {
    let data_len = (rec.samples.len() * 2) as u32;
    let mut bytes = Vec::with_capacity(44 + data_len as usize);
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    bytes.extend_from_slice(&1u16.to_le_bytes()); // mono
    bytes.extend_from_slice(&rec.rate.to_le_bytes());
    bytes.extend_from_slice(&(rec.rate * 2).to_le_bytes()); // byte rate
    bytes.extend_from_slice(&2u16.to_le_bytes()); // block align
    bytes.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    for s in &rec.samples {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    let tmp = path.with_extension("wav.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_valid() {
        let rec = Recording { samples: vec![0, 1000, -1000, 0], rate: 48_000 };
        let path = std::env::temp_dir().join("tamka-test.wav");
        write_wav(&path, &rec).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 8);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 48_000);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 8);
        let _ = std::fs::remove_file(path);
    }
}
