//! Microphone capture with AVAudioEngine. The mic is open only while recording.
//!
//! Audio arrives on Core Audio's thread in ~85 ms buffers. Channel 0 is kept as
//! 16-bit samples at the device's own rate; the engine helper resamples to 16 kHz.

use std::cell::RefCell;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2_avf_audio::{AVAudioEngine, AVAudioPCMBuffer, AVAudioTime};

type TapBlock = RcBlock<dyn Fn(NonNull<AVAudioPCMBuffer>, NonNull<AVAudioTime>)>;

struct Active {
    engine: Retained<AVAudioEngine>,
    _tap: TapBlock,
    rate: u32,
    started: Instant,
}

static SAMPLES: Mutex<Vec<i16>> = Mutex::new(Vec::new());
/// RMS of the latest buffer (f32 bits), for the recording indicator.
static LEVEL: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
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

pub fn start() -> Result<(), String> {
    if is_recording() {
        return Ok(());
    }
    let engine = unsafe { AVAudioEngine::new() };
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
        *a.borrow_mut() = Some(Active { engine, _tap: tap, rate: rate.round() as u32, started: Instant::now() })
    });
    Ok(())
}

/// Stop and return what was recorded.
pub fn stop() -> Option<Recording> {
    let active = ACTIVE.with(|a| a.borrow_mut().take())?;
    unsafe {
        active.engine.inputNode().removeTapOnBus(0);
        active.engine.stop();
    }
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
