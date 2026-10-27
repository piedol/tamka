//! Microphone capture with AVAudioEngine. The mic is open only while recording.
//!
//! Tamka records from the microphone chosen in Settings while it's connected, and from
//! the system default input otherwise.
//!
//! Audio arrives on Core Audio's thread in ~85 ms buffers. The channels are mixed to one
//! and kept as 16-bit samples at the device's own rate; the engine helper resamples to 16 kHz.
//!
//! Building an engine and its input node takes 250 ms or more, so one is kept built
//! and prepared between recordings (`prewarm`). A prepared engine doesn't open the
//! mic; only `start` does. It is rebuilt when the input device changes.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::msg_send;
use objc2_avf_audio::{AVAudioEngine, AVAudioPCMBuffer, AVAudioTime};
use objc2_core_foundation::{CFRetained, CFString};
use objc2_foundation::{NSNotification, NSNotificationCenter, NSObjectProtocol, NSOperationQueue, NSString};

use crate::settings;

type TapBlock = RcBlock<dyn Fn(NonNull<AVAudioPCMBuffer>, NonNull<AVAudioTime>)>;

struct Active {
    engine: Retained<AVAudioEngine>,
    device: u32,
    _tap: TapBlock,
    /// Watches for the engine stopping itself because its device went away or changed format.
    observer: Retained<ProtocolObject<dyn NSObjectProtocol>>,
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
    /// Set when the recording engine stopped itself; that engine isn't reused.
    static CHANGED: Cell<bool> = const { Cell::new(false) };
    /// Engines set aside and released a little later (see `retire`).
    static RETIRED: RefCell<Vec<(Instant, Retained<AVAudioEngine>)>> = const { RefCell::new(Vec::new()) };
}

/// How long a retired engine is kept before it's released.
const LINGER: Duration = Duration::from_secs(10);

// Core Audio property selectors and scopes (four-character codes).
const SYSTEM_OBJECT: u32 = 1;
const DEFAULT_INPUT: u32 = 0x6449_6e20; // 'dIn '
const DEVICES: u32 = 0x6465_7623; // 'dev#'
const DEVICE_UID: u32 = 0x7569_6420; // 'uid '
const OBJECT_NAME: u32 = 0x6c6e_616d; // 'lnam'
const STREAMS: u32 = 0x7374_6d23; // 'stm#'
const IS_HIDDEN: u32 = 0x6869_646e; // 'hidn'
const SCOPE_GLOBAL: u32 = 0x676c_6f62; // 'glob'
const SCOPE_INPUT: u32 = 0x696e_7074; // 'inpt'
/// kAudioOutputUnitProperty_CurrentDevice: which device an input node's I/O unit records from.
const AU_CURRENT_DEVICE: u32 = 2000;

#[repr(C)]
struct PropertyAddress {
    selector: u32,
    scope: u32,
    element: u32,
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(
        object: u32,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
    ) -> i32;
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const PropertyAddress,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
    fn AudioObjectAddPropertyListenerBlock(
        object: u32,
        address: *const PropertyAddress,
        queue: *const c_void,
        listener: *const block2::Block<dyn Fn(u32, *const c_void)>,
    ) -> i32;
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioUnitSetProperty(unit: *mut c_void, id: u32, scope: u32, element: u32, data: *const c_void, size: u32)
    -> i32;
}

/// An AudioUnit, as `-[AVAudioIONode audioUnit]` returns it.
#[repr(C)]
struct AudioUnitRecord {
    _private: [u8; 0],
}

// SAFETY: matches the method's declared return type, `^{ComponentInstanceRecord=[1q]}`.
unsafe impl objc2::encode::RefEncode for AudioUnitRecord {
    const ENCODING_REF: objc2::encode::Encoding = objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct(
        "ComponentInstanceRecord",
        &[objc2::encode::Encoding::Array(1, &objc2::encode::Encoding::LongLong)],
    ));
}

fn address(selector: u32, scope: u32) -> PropertyAddress {
    PropertyAddress { selector, scope, element: 0 }
}

fn data_size(object: u32, selector: u32, scope: u32) -> u32 {
    let mut size = 0u32;
    let status =
        unsafe { AudioObjectGetPropertyDataSize(object, &address(selector, scope), 0, std::ptr::null(), &mut size) };
    if status == 0 { size } else { 0 }
}

fn get_u32(object: u32, selector: u32, scope: u32) -> Option<u32> {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(object, &address(selector, scope), 0, std::ptr::null(), &mut size, (&raw mut value).cast())
    };
    (status == 0).then_some(value)
}

fn get_string(object: u32, selector: u32) -> Option<String> {
    let mut value: *const CFString = std::ptr::null();
    let mut size = std::mem::size_of::<*const CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            &address(selector, SCOPE_GLOBAL),
            0,
            std::ptr::null(),
            &mut size,
            (&raw mut value).cast(),
        )
    };
    // SAFETY: Core Audio hands back a CFString the caller owns.
    let value = unsafe { CFRetained::from_raw(NonNull::new(value.cast_mut())?) };
    (status == 0).then(|| value.to_string())
}

/// A microphone or other audio input.
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub id: u32,
    /// Core Audio's unique id, the same each time the device connects.
    pub uid: String,
    pub name: String,
}

/// The inputs connected now, leaving out hidden ones and the aggregate devices
/// AVAudioEngine makes for itself while it records.
pub fn inputs() -> Vec<Input> {
    let mut size = data_size(SYSTEM_OBJECT, DEVICES, SCOPE_GLOBAL);
    let mut ids = vec![0u32; size as usize / std::mem::size_of::<u32>()];
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &address(DEVICES, SCOPE_GLOBAL),
            0,
            std::ptr::null(),
            &mut size,
            ids.as_mut_ptr().cast(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids.truncate(size as usize / std::mem::size_of::<u32>());
    ids.into_iter()
        .filter(|&id| data_size(id, STREAMS, SCOPE_INPUT) > 0 && get_u32(id, IS_HIDDEN, SCOPE_GLOBAL) != Some(1))
        .filter_map(|id| {
            let uid = get_string(id, DEVICE_UID)?;
            if uid.starts_with("CADefaultDeviceAggregate") {
                return None;
            }
            let name = get_string(id, OBJECT_NAME).unwrap_or_else(|| uid.clone());
            Some(Input { id, uid, name })
        })
        .collect()
}

/// The system's default input device (Core Audio id), or 0 if there isn't one.
fn default_input() -> u32 {
    get_u32(SYSTEM_OBJECT, DEFAULT_INPUT, SCOPE_GLOBAL).unwrap_or(0)
}

/// The name of the system's default input, if there is one.
pub fn default_input_name() -> Option<String> {
    match default_input() {
        0 => None,
        id => get_string(id, OBJECT_NAME),
    }
}

/// The input Tamka records from: the chosen microphone while it's connected, else the
/// system default.
fn current_input() -> u32 {
    let uid = settings::get().mic_uid;
    if !uid.is_empty()
        && let Some(input) = inputs().into_iter().find(|i| i.uid == uid)
    {
        return input.id;
    }
    default_input()
}

/// The name of the input the next dictation will use.
pub fn current_input_name() -> Option<String> {
    match current_input() {
        0 => None,
        id => get_string(id, OBJECT_NAME),
    }
}

/// Record from `input` from now on (None: the system default).
pub fn choose(input: Option<&Input>) {
    settings::update(|s| {
        s.mic_uid = input.map(|i| i.uid.clone()).unwrap_or_default();
        s.mic_name = input.map(|i| i.name.clone()).unwrap_or_default();
    });
    prewarm();
}

/// Run `f` on the main thread whenever an input connects or disconnects, or the default
/// input changes (AirPods connect, a mic is plugged in), so a matching engine is ready in time.
pub fn watch_inputs(f: fn()) {
    for selector in [DEFAULT_INPUT, DEVICES] {
        let block = RcBlock::new(move |_: u32, _: *const c_void| crate::util::main_async(f));
        // A null queue runs the block on Core Audio's own thread; it only hops to main.
        let status = unsafe {
            AudioObjectAddPropertyListenerBlock(
                SYSTEM_OBJECT,
                &address(selector, SCOPE_GLOBAL),
                std::ptr::null(),
                RcBlock::as_ptr(&block),
            )
        };
        if status == 0 {
            // Core Audio copies the block and keeps it for as long as Tamka runs.
            std::mem::forget(block);
        }
    }
}

fn build(device: u32) -> Warm {
    let engine = unsafe { AVAudioEngine::new() };
    // A device that is coming or going can make AVAudioEngine raise; `open` then retries.
    let built = objc2::exception::catch(AssertUnwindSafe(|| unsafe {
        let input = engine.inputNode();
        if device != 0 {
            let unit: *mut AudioUnitRecord = msg_send![&*input, audioUnit];
            if !unit.is_null() {
                let status =
                    AudioUnitSetProperty(unit.cast(), AU_CURRENT_DEVICE, 0, 0, (&raw const device).cast(), 4);
                if status != 0 {
                    crate::util::log(format!("couldn't select input device {device}: error {status}"));
                }
            }
        }
        engine.prepare();
    }));
    if let Err(e) = built {
        crate::util::log(format!("couldn't prepare the audio engine: {e:?}"));
    }
    Warm { engine, device }
}

/// Stop an engine and release it a little later. When a device goes away, Core Audio can
/// still be running one of the engine's own property listeners, and releasing the engine
/// under it crashes.
fn retire(engine: Retained<AVAudioEngine>) {
    let _ = objc2::exception::catch(AssertUnwindSafe(|| unsafe { engine.stop() }));
    RETIRED.with(|r| {
        let mut r = r.borrow_mut();
        r.retain(|(t, _)| t.elapsed() < LINGER);
        r.push((Instant::now(), engine));
    });
    crate::util::main_after(LINGER.as_secs_f64() + 1.0, release_retired);
}

fn release_retired() {
    RETIRED.with(|r| r.borrow_mut().retain(|(t, _)| t.elapsed() < LINGER));
}

/// Build and prepare an engine now so the next `start` is quick. Cheap if one is ready.
pub fn prewarm() {
    if is_recording() {
        return;
    }
    let device = current_input();
    let old = WARM.with(|w| w.borrow_mut().take_if(|w| w.device != device));
    if let Some(old) = old {
        retire(old.engine);
    }
    if WARM.with(|w| w.borrow().is_none()) {
        let t = Instant::now();
        let warm = build(device);
        WARM.with(|w| *w.borrow_mut() = Some(warm));
        crate::util::log(format!("audio engine ready in {} ms", t.elapsed().as_millis()));
    }
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
    let device = current_input();
    if device == 0 {
        return Err("No microphone is available.".into());
    }
    let warm = WARM.with(|w| w.borrow_mut().take());
    let (mut warm, reused) = match warm {
        Some(w) if w.device == device => (Some(w), true),
        Some(w) => {
            retire(w.engine);
            (None, false)
        }
        None => (None, false),
    };
    // A prepared engine can be stale if the device changed format since (AirPods switching
    // to their headset mode, say). Then build a fresh one and try once more.
    let mut last_error = String::new();
    for _ in 0..2 {
        let Warm { engine, device } = warm.take().unwrap_or_else(|| build(device));
        match open(&engine) {
            Ok((tap, rate)) => {
                let observer = observe(&engine);
                CHANGED.with(|c| c.set(false));
                ACTIVE.with(|a| {
                    *a.borrow_mut() =
                        Some(Active { engine, device, _tap: tap, observer, rate, started: Instant::now() })
                });
                return Ok(reused);
            }
            Err(e) => {
                crate::util::log(format!("couldn't open the microphone: {e}"));
                retire(engine);
                last_error = e;
            }
        }
    }
    Err(last_error)
}

/// Install the tap and start `engine`, returning the tap and the sample rate.
fn open(engine: &AVAudioEngine) -> Result<(TapBlock, u32), String> {
    let input = unsafe { engine.inputNode() };
    let format = unsafe { input.outputFormatForBus(0) };
    let hardware = unsafe { input.inputFormatForBus(0) };
    let rate = unsafe { format.sampleRate() };
    let channels = unsafe { format.channelCount() };
    if rate <= 0.0 || channels == 0 {
        return Err("No microphone is available.".into());
    }
    // Installing a tap at a sample rate the hardware no longer runs at raises an exception.
    // (Channel counts can differ: the node hands a mono mic over as two channels.)
    if unsafe { hardware.sampleRate() } != rate {
        return Err("The microphone changed format.".into());
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
        let mono = mix_down(&channels_of(unsafe { buffer.as_ref() }));
        if mono.is_empty() {
            return;
        }
        let mut sum = 0.0f32;
        let mut out = SAMPLES.lock().unwrap();
        for &s in &mono {
            sum += s * s;
            out.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        if sum > 0.0 {
            LIVE_AT.lock().unwrap().get_or_insert_with(Instant::now);
        }
        LEVEL.store((sum / mono.len() as f32).sqrt().to_bits(), Ordering::Relaxed);
    });

    let started = objc2::exception::catch(AssertUnwindSafe(|| unsafe {
        input.installTapOnBus_bufferSize_format_block(0, 4096, Some(&format), RcBlock::as_ptr(&tap));
        engine.prepare();
        engine.startAndReturnError()
    }));
    match started {
        Ok(Ok(())) => Ok((tap, rate.round() as u32)),
        Ok(Err(e)) => {
            let _ = objc2::exception::catch(AssertUnwindSafe(|| unsafe { input.removeTapOnBus(0) }));
            Err(e.localizedDescription().to_string())
        }
        Err(exception) => {
            let _ = objc2::exception::catch(AssertUnwindSafe(|| unsafe { input.removeTapOnBus(0) }));
            Err(exception.map_or_else(|| "unknown exception".to_string(), |e| format!("{e:?}")))
        }
    }
}

/// Call `app::input_lost` if `engine` stops itself mid-recording, which it does when its
/// device disconnects or changes format.
fn observe(engine: &AVAudioEngine) -> Retained<ProtocolObject<dyn NSObjectProtocol>> {
    let name = NSString::from_str("AVAudioEngineConfigurationChangeNotification");
    let block = RcBlock::new(|_: NonNull<NSNotification>| {
        let stopped = ACTIVE.with(|a| a.borrow().as_ref().is_some_and(|a| !unsafe { a.engine.isRunning() }));
        if stopped {
            CHANGED.with(|c| c.set(true));
            crate::app::input_lost();
        }
    });
    unsafe {
        NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(&name),
            Some(engine),
            Some(&NSOperationQueue::mainQueue()),
            &block,
        )
    }
}

/// Each channel of `buffer` as floats.
fn channels_of(buffer: &AVAudioPCMBuffer) -> Vec<Vec<f32>> {
    let frames = unsafe { buffer.frameLength() } as usize;
    let count = unsafe { buffer.format().channelCount() } as usize;
    // Interleaved buffers hold every channel in the first pointer, `stride` apart.
    let stride = unsafe { buffer.stride() }.max(1);
    let interleaved = stride > 1;
    let float = unsafe { buffer.floatChannelData() };
    let int16 = unsafe { buffer.int16ChannelData() };
    if frames == 0 || count == 0 || (float.is_null() && int16.is_null()) {
        return Vec::new();
    }
    (0..count)
        .map(|ch| {
            let (pointer, offset) = if interleaved { (0, ch) } else { (ch, 0) };
            (0..frames)
                .map(|i| {
                    let at = i * stride + offset;
                    // SAFETY: the buffer holds `frames` frames of `count` channels.
                    unsafe {
                        if !float.is_null() {
                            *(*float.add(pointer)).as_ptr().add(at)
                        } else {
                            *(*int16.add(pointer)).as_ptr().add(at) as f32 / 32768.0
                        }
                    }
                })
                .collect()
        })
        .collect()
}

/// Mix channels down to one, weighting each by its loudness. A mic that's on one channel
/// only (a DJI receiver in stereo with one transmitter, an audio interface with one mic
/// plugged in) keeps its full level, and identical channels mix to themselves.
fn mix_down(channels: &[Vec<f32>]) -> Vec<f32> {
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    if channels.len() == 1 || frames == 0 {
        return channels.first().cloned().unwrap_or_default();
    }
    let rms: Vec<f32> =
        channels.iter().map(|c| (c[..frames].iter().map(|s| s * s).sum::<f32>() / frames as f32).sqrt()).collect();
    let total: f32 = rms.iter().sum();
    let weights: Vec<f32> = if total > 0.0 {
        rms.iter().map(|r| r / total).collect()
    } else {
        vec![1.0 / channels.len() as f32; channels.len()]
    };
    (0..frames).map(|i| channels.iter().zip(&weights).map(|(c, w)| c[i] * w).sum()).collect()
}

/// Stop and return what was recorded.
pub fn stop() -> Option<Recording> {
    let active = ACTIVE.with(|a| a.borrow_mut().take())?;
    unsafe { NSNotificationCenter::defaultCenter().removeObserver(active.observer.as_ref()) };
    let _ = objc2::exception::catch(AssertUnwindSafe(|| unsafe { active.engine.inputNode().removeTapOnBus(0) }));
    if CHANGED.with(Cell::take) {
        // It stopped because its device went away or changed; build a new one for next time.
        retire(active.engine);
        crate::util::main_after(0.5, prewarm);
    } else {
        let _ = objc2::exception::catch(AssertUnwindSafe(|| unsafe {
            active.engine.stop();
            active.engine.prepare();
        }));
        // Keep the engine for next time; `start` checks it still matches the input device.
        WARM.with(|w| *w.borrow_mut() = Some(Warm { engine: active.engine, device: active.device }));
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
    fn a_mic_on_one_channel_keeps_its_level() {
        let speech = vec![0.5, -0.5, 0.25, -0.25];
        let mixed = mix_down(&[vec![0.0; 4], speech.clone()]);
        for (m, s) in mixed.iter().zip(&speech) {
            assert!((m - s).abs() < 1e-6);
        }
    }

    #[test]
    fn identical_channels_mix_to_themselves() {
        let speech = vec![0.5, -0.5, 0.25, -0.25];
        let mixed = mix_down(&[speech.clone(), speech.clone()]);
        for (m, s) in mixed.iter().zip(&speech) {
            assert!((m - s).abs() < 1e-6);
        }
    }

    #[test]
    fn silence_mixes_to_silence() {
        assert_eq!(mix_down(&[vec![0.0; 3], vec![0.0; 3]]), vec![0.0; 3]);
        assert_eq!(mix_down(&[vec![0.1, 0.2]]), vec![0.1, 0.2]);
        assert!(mix_down(&[]).is_empty());
    }

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
