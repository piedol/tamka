//! Microphone capture through Core Audio's HAL input unit. The mic is open only while recording.
//!
//! Tamka records from the microphone chosen in Settings while it's connected, and from
//! the system default input otherwise. The unit only does input, so a mic and a different
//! output device (a USB receiver while AirPods play) never get joined into an aggregate
//! device; AVAudioEngine did that, and the aggregate delivered silence.
//!
//! Audio arrives on Core Audio's thread in small buffers. The channels are mixed to one and
//! kept as 16-bit samples at the device's own rate; the engine helper resamples to 16 kHz.
//!
//! A unit is kept built and initialized between recordings (`prewarm`), which doesn't open
//! the mic; only `start` does. It is rebuilt when the input device changes.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2_core_foundation::{CFRetained, CFString};

use crate::settings;

static SAMPLES: Mutex<Vec<i16>> = Mutex::new(Vec::new());
/// RMS of the latest buffer (f32 bits), for the recording indicator.
static LEVEL: AtomicU32 = AtomicU32::new(0);
/// When the first buffer with any sound in it arrived. Bluetooth mics send silence
/// while they switch over, so this is when the mic is really listening.
static LIVE_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// Set once when the input fails mid-recording, so the main thread hears about it once.
static FAILED: AtomicBool = AtomicBool::new(false);

thread_local! {
    static ACTIVE: RefCell<Option<Active>> = const { RefCell::new(None) };
    static WARM: RefCell<Option<Unit>> = const { RefCell::new(None) };
    /// Set when the recording's device went away or failed; that unit isn't reused.
    static CHANGED: Cell<bool> = const { Cell::new(false) };
}

struct Active {
    unit: Unit,
    /// Calls `lost` if the device disconnects.
    alive: Option<Listener>,
    started: Instant,
}

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

const IS_ALIVE: u32 = 0x6c69_766e; // 'livn'

// The HAL output unit ('auou' 'ahal' 'appl'), used for input only.
const AU_TYPE_OUTPUT: u32 = 0x6175_6f75;
const AU_SUBTYPE_HAL: u32 = 0x6168_616c;
const AU_APPLE: u32 = 0x6170_706c;
const AU_ENABLE_IO: u32 = 2003;
const AU_CURRENT_DEVICE: u32 = 2000;
const AU_INPUT_CALLBACK: u32 = 2005;
const AU_STREAM_FORMAT: u32 = 8;
const AU_MAX_FRAMES: u32 = 14;
const AU_SCOPE_GLOBAL: u32 = 0;
const AU_SCOPE_INPUT: u32 = 1;
const AU_SCOPE_OUTPUT: u32 = 2;
/// Element 1 is the input side of the unit, element 0 the output side.
const AU_INPUT: u32 = 1;
const AU_OUTPUT: u32 = 0;
const LINEAR_PCM: u32 = 0x6c70_636d; // 'lpcm'
/// kAudioFormatFlagIsFloat | IsPacked | IsNonInterleaved.
const FLOAT_NON_INTERLEAVED: u32 = 1 | 8 | 32;
/// Channels mixed from a device; more than this and the rest are left out.
const MAX_CHANNELS: usize = 8;

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

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct StreamFormat {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct ComponentDescription {
    kind: u32,
    sub_kind: u32,
    manufacturer: u32,
    flags: u32,
    flags_mask: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioBuffer {
    channels: u32,
    bytes: u32,
    data: *mut c_void,
}

#[repr(C)]
struct BufferList {
    count: u32,
    buffers: [AudioBuffer; MAX_CHANNELS],
}

type InputProc = unsafe extern "C" fn(*mut c_void, *mut u32, *const c_void, u32, u32, *mut BufferList) -> i32;

#[repr(C)]
struct RenderCallback {
    input: InputProc,
    refcon: *mut c_void,
}

#[link(name = "AudioToolbox", kind = "framework")]
unsafe extern "C" {
    fn AudioComponentFindNext(after: *mut c_void, description: *const ComponentDescription) -> *mut c_void;
    fn AudioComponentInstanceNew(component: *mut c_void, unit: *mut *mut c_void) -> i32;
    fn AudioComponentInstanceDispose(unit: *mut c_void) -> i32;
    fn AudioUnitSetProperty(unit: *mut c_void, id: u32, scope: u32, element: u32, data: *const c_void, size: u32)
    -> i32;
    fn AudioUnitGetProperty(unit: *mut c_void, id: u32, scope: u32, element: u32, data: *mut c_void, size: *mut u32)
    -> i32;
    fn AudioUnitInitialize(unit: *mut c_void) -> i32;
    fn AudioUnitUninitialize(unit: *mut c_void) -> i32;
    fn AudioOutputUnitStart(unit: *mut c_void) -> i32;
    fn AudioOutputUnitStop(unit: *mut c_void) -> i32;
    fn AudioUnitRender(
        unit: *mut c_void,
        flags: *mut u32,
        time: *const c_void,
        bus: u32,
        frames: u32,
        data: *mut BufferList,
    ) -> i32;
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectRemovePropertyListenerBlock(
        object: u32,
        address: *const PropertyAddress,
        queue: *const c_void,
        listener: *const block2::Block<dyn Fn(u32, *const c_void)>,
    ) -> i32;
}

fn checked(what: &str, status: i32) -> Result<(), String> {
    if status == 0 {
        return Ok(());
    }
    // OSStatus values are often four printable characters.
    let bytes = status.to_be_bytes();
    let code = if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        format!("'{}'", String::from_utf8_lossy(&bytes))
    } else {
        status.to_string()
    };
    Err(format!("{what} failed (Core Audio error {code})"))
}

/// What the input callback needs: the unit to pull audio from, and room for each channel.
struct Callback {
    unit: *mut c_void,
    channels: usize,
    buffers: Vec<Vec<f32>>,
}

/// An input device opened through a HAL unit, initialized and ready to start.
struct Unit {
    au: *mut c_void,
    device: u32,
    rate: u32,
    /// Boxed so its address, which the callback holds, stays put when the unit moves.
    callback: Box<Callback>,
}

impl Unit {
    fn open(device: u32) -> Result<Unit, String> {
        let description =
            ComponentDescription { kind: AU_TYPE_OUTPUT, sub_kind: AU_SUBTYPE_HAL, manufacturer: AU_APPLE, flags: 0, flags_mask: 0 };
        let component = unsafe { AudioComponentFindNext(std::ptr::null_mut(), &description) };
        if component.is_null() {
            return Err("Core Audio's input unit is missing.".into());
        }
        let mut au = std::ptr::null_mut();
        checked("Creating the input unit", unsafe { AudioComponentInstanceNew(component, &mut au) })?;
        // From here on, dropping `unit` disposes of the AudioUnit.
        let mut unit = Unit {
            au,
            device,
            rate: 0,
            callback: Box::new(Callback { unit: au, channels: 0, buffers: Vec::new() }),
        };
        let set_u32 = |id, scope, element, value: u32| unsafe {
            AudioUnitSetProperty(au, id, scope, element, (&raw const value).cast(), 4)
        };
        checked("Turning on input", set_u32(AU_ENABLE_IO, AU_SCOPE_INPUT, AU_INPUT, 1))?;
        checked("Turning off output", set_u32(AU_ENABLE_IO, AU_SCOPE_OUTPUT, AU_OUTPUT, 0))?;
        checked("Choosing the microphone", set_u32(AU_CURRENT_DEVICE, AU_SCOPE_GLOBAL, 0, device))?;
        let hardware = unit.hardware_format()?;
        let rate = hardware.sample_rate;
        let channels = (hardware.channels_per_frame as usize).min(MAX_CHANNELS);
        if rate <= 0.0 || channels == 0 {
            return Err("No microphone is available.".into());
        }
        // Float, one buffer per channel, at the device's rate: the unit converts the sample
        // format and layout, but not the rate.
        let client = StreamFormat {
            sample_rate: rate,
            format_id: LINEAR_PCM,
            format_flags: FLOAT_NON_INTERLEAVED,
            bytes_per_packet: 4,
            frames_per_packet: 1,
            bytes_per_frame: 4,
            channels_per_frame: channels as u32,
            bits_per_channel: 32,
            reserved: 0,
        };
        checked("Setting the recording format", unsafe {
            AudioUnitSetProperty(
                au,
                AU_STREAM_FORMAT,
                AU_SCOPE_OUTPUT,
                AU_INPUT,
                (&raw const client).cast(),
                std::mem::size_of::<StreamFormat>() as u32,
            )
        })?;
        let mut max_frames = 0u32;
        let mut size = 4u32;
        unsafe { AudioUnitGetProperty(au, AU_MAX_FRAMES, AU_SCOPE_GLOBAL, 0, (&raw mut max_frames).cast(), &mut size) };
        unit.callback.channels = channels;
        unit.callback.buffers = vec![vec![0.0; max_frames.max(4096) as usize]; channels];
        let callback = RenderCallback { input: on_input, refcon: (&raw mut *unit.callback).cast() };
        checked("Setting the input callback", unsafe {
            AudioUnitSetProperty(
                au,
                AU_INPUT_CALLBACK,
                AU_SCOPE_GLOBAL,
                0,
                (&raw const callback).cast(),
                std::mem::size_of::<RenderCallback>() as u32,
            )
        })?;
        checked("Preparing the microphone", unsafe { AudioUnitInitialize(au) })?;
        unit.rate = rate.round() as u32;
        Ok(unit)
    }

    /// The device's own input format, as the unit sees it now.
    fn hardware_format(&self) -> Result<StreamFormat, String> {
        let mut format = StreamFormat::default();
        let mut size = std::mem::size_of::<StreamFormat>() as u32;
        checked("Reading the microphone's format", unsafe {
            AudioUnitGetProperty(self.au, AU_STREAM_FORMAT, AU_SCOPE_INPUT, AU_INPUT, (&raw mut format).cast(), &mut size)
        })?;
        Ok(format)
    }
}

impl Drop for Unit {
    fn drop(&mut self) {
        unsafe {
            AudioOutputUnitStop(self.au);
            AudioUnitUninitialize(self.au);
            AudioComponentInstanceDispose(self.au);
        }
    }
}

/// Pulls each buffer of input from the unit and adds it to the recording. Runs on Core
/// Audio's thread.
unsafe extern "C" fn on_input(
    refcon: *mut c_void,
    flags: *mut u32,
    time: *const c_void,
    bus: u32,
    frames: u32,
    _data: *mut BufferList,
) -> i32 {
    // SAFETY: `refcon` is the unit's boxed `Callback`, which outlives the running unit.
    let callback = unsafe { &mut *refcon.cast::<Callback>() };
    let n = frames as usize;
    if callback.buffers.first().is_none_or(|b| b.len() < n) {
        return 0;
    }
    let empty = AudioBuffer { channels: 1, bytes: 0, data: std::ptr::null_mut() };
    let mut list = BufferList { count: callback.channels as u32, buffers: [empty; MAX_CHANNELS] };
    for (slot, buffer) in list.buffers.iter_mut().zip(callback.buffers.iter_mut()) {
        *slot = AudioBuffer { channels: 1, bytes: frames * 4, data: buffer.as_mut_ptr().cast() };
    }
    let status = unsafe { AudioUnitRender(callback.unit, flags, time, bus, frames, &mut list) };
    if status != 0 {
        if !FAILED.swap(true, Ordering::Relaxed) {
            crate::util::main_async(lost);
        }
        return status;
    }
    let channels: Vec<&[f32]> = list.buffers[..callback.channels]
        .iter()
        .filter(|b| !b.data.is_null())
        // SAFETY: the unit filled `frames` floats into each buffer.
        .map(|b| unsafe { std::slice::from_raw_parts(b.data.cast::<f32>(), n.min(b.bytes as usize / 4)) })
        .collect();
    add(&mix_down(&channels));
    0
}

/// Append a mixed buffer to the recording and update the level.
fn add(mono: &[f32]) {
    if mono.is_empty() {
        return;
    }
    let mut sum = 0.0f32;
    let mut out = SAMPLES.lock().unwrap();
    for &s in mono {
        sum += s * s;
        out.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
    }
    if sum > 0.0 {
        LIVE_AT.lock().unwrap().get_or_insert_with(Instant::now);
    }
    LEVEL.store((sum / mono.len() as f32).sqrt().to_bits(), Ordering::Relaxed);
}

/// Mix channels down to one, weighting each by its loudness. A mic that's on one channel
/// only (a DJI receiver in stereo with one transmitter, an audio interface with one mic
/// plugged in) keeps its full level, and identical channels mix to themselves.
fn mix_down(channels: &[&[f32]]) -> Vec<f32> {
    let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
    if channels.len() == 1 || frames == 0 {
        return channels.first().map(|c| c.to_vec()).unwrap_or_default();
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

/// A Core Audio property listener, removed when dropped.
struct Listener {
    object: u32,
    selector: u32,
    block: RcBlock<dyn Fn(u32, *const c_void)>,
}

impl Listener {
    /// Run `f` on the main thread when `selector` changes on `object`.
    fn add(object: u32, selector: u32, f: fn()) -> Option<Listener> {
        let block: RcBlock<dyn Fn(u32, *const c_void)> =
            RcBlock::new(move |_: u32, _: *const c_void| crate::util::main_async(f));
        let status = unsafe {
            AudioObjectAddPropertyListenerBlock(
                object,
                &address(selector, SCOPE_GLOBAL),
                std::ptr::null(),
                RcBlock::as_ptr(&block),
            )
        };
        (status == 0).then_some(Listener { object, selector, block })
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        unsafe {
            AudioObjectRemovePropertyListenerBlock(
                self.object,
                &address(self.selector, SCOPE_GLOBAL),
                std::ptr::null(),
                RcBlock::as_ptr(&self.block),
            );
        }
    }
}

/// The recording's device disconnected or stopped delivering audio: tell the app, which
/// ends the dictation with what was heard.
fn lost() {
    if is_recording() {
        CHANGED.with(|c| c.set(true));
        crate::app::input_lost();
    }
}

/// The device the recording uses died (or, for the 'livn' listener, changed liveness).
fn device_changed() {
    let gone = ACTIVE.with(|a| {
        a.borrow().as_ref().is_some_and(|a| get_u32(a.unit.device, IS_ALIVE, SCOPE_GLOBAL) != Some(1))
    });
    if gone {
        lost();
    }
}

/// Build and initialize a unit now so the next `start` is quick. Cheap if one is ready.
pub fn prewarm() {
    if is_recording() {
        return;
    }
    let device = current_input();
    WARM.with(|w| {
        let mut w = w.borrow_mut();
        if w.as_ref().is_some_and(|u| u.device == device) || device == 0 {
            return;
        }
        *w = None;
        let t = Instant::now();
        match Unit::open(device) {
            Ok(unit) => {
                *w = Some(unit);
                crate::util::log(format!("audio input ready in {} ms", t.elapsed().as_millis()));
            }
            Err(e) => crate::util::log(format!("couldn't prepare the microphone: {e}")),
        }
    });
}

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

/// Open the mic and start collecting audio. Returns whether a prepared unit was used.
pub fn start() -> Result<bool, String> {
    if is_recording() {
        return Ok(true);
    }
    let device = current_input();
    if device == 0 {
        return Err("No microphone is available.".into());
    }
    let warm = WARM.with(|w| w.borrow_mut().take()).filter(|u| u.device == device);
    let reused = warm.is_some();
    // A prepared unit can be stale if the device changed rate since (AirPods switching to
    // their headset mode, say). Then build a fresh one and try once more.
    let mut warm = warm;
    let mut last_error = String::new();
    for _ in 0..2 {
        let unit = match warm.take() {
            Some(u) => u,
            None => match Unit::open(device) {
                Ok(u) => u,
                Err(e) => {
                    last_error = e;
                    continue;
                }
            },
        };
        let current = unit.hardware_format().map(|f| f.sample_rate.round() as u32).unwrap_or(0);
        if current != unit.rate {
            last_error = "The microphone changed format.".into();
            continue;
        }
        {
            let mut s = SAMPLES.lock().unwrap();
            s.clear();
            s.reserve(unit.rate as usize * 60);
        }
        LEVEL.store(0, Ordering::Relaxed);
        *LIVE_AT.lock().unwrap() = None;
        FAILED.store(false, Ordering::Relaxed);
        if let Err(e) = checked("Starting the microphone", unsafe { AudioOutputUnitStart(unit.au) }) {
            last_error = e;
            continue;
        }
        CHANGED.with(|c| c.set(false));
        let alive = Listener::add(device, IS_ALIVE, device_changed);
        ACTIVE.with(|a| *a.borrow_mut() = Some(Active { unit, alive, started: Instant::now() }));
        return Ok(reused);
    }
    crate::util::log(format!("couldn't open the microphone: {last_error}"));
    Err(last_error)
}

/// Stop and return what was recorded.
pub fn stop() -> Option<Recording> {
    let active = ACTIVE.with(|a| a.borrow_mut().take())?;
    drop(active.alive);
    unsafe { AudioOutputUnitStop(active.unit.au) };
    let rate = active.unit.rate;
    if CHANGED.with(Cell::take) {
        // It stopped because its device went away or failed; build a new one for next time.
        drop(active.unit);
        crate::util::main_after(0.5, prewarm);
    } else {
        // Keep the unit for next time; `start` checks it still matches the input device.
        WARM.with(|w| *w.borrow_mut() = Some(active.unit));
    }
    let samples = std::mem::take(&mut *SAMPLES.lock().unwrap());
    Some(Recording { samples, rate })
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
        let mixed = mix_down(&[&[0.0; 4], &speech]);
        for (m, s) in mixed.iter().zip(&speech) {
            assert!((m - s).abs() < 1e-6);
        }
    }

    #[test]
    fn identical_channels_mix_to_themselves() {
        let speech = vec![0.5, -0.5, 0.25, -0.25];
        let mixed = mix_down(&[&speech, &speech]);
        for (m, s) in mixed.iter().zip(&speech) {
            assert!((m - s).abs() < 1e-6);
        }
    }

    #[test]
    fn silence_mixes_to_silence() {
        assert_eq!(mix_down(&[&[0.0; 3], &[0.0; 3]]), vec![0.0; 3]);
        assert_eq!(mix_down(&[&[0.1, 0.2]]), vec![0.1, 0.2]);
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
