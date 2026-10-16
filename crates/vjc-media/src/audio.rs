//! Clip audio: FFmpeg decode, resample, per-layer mix, cpal output.
//!
//! Each playing file clip gets a `Voice` (lock-free SPSC ring of interleaved stereo f32 at
//! the device rate, plus atomic volume/pan/audible) and a decode thread that keeps the ring
//! topped up to ~`TARGET_LEAD` seconds. The cpal callback never allocates and never blocks:
//! it `try_lock`s the voice list (silence for that buffer if the lock is busy, which only
//! happens while a clip starts or stops) and mixes straight from the rings.
//!
//! Sync rule: audio follows the clip playhead only at forward 1x speed. At any other speed,
//! backward, paused or when the playhead jumps, the audio is muted (ring flushed) and
//! re-seeks to the playhead once it is audible again. No pitch-shifting or time-stretching.

use crate::Shared;
use crate::audio_fx::{Buses, FxChain, MAX_BUSES};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg_next as ff;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Ring capacity per voice, in seconds of audio.
const RING_SECONDS: f64 = 0.5;
/// Decoder keeps the ring filled up to this much audio (the audio latency vs the video).
const TARGET_LEAD: f64 = 0.15;
/// Frames mixed per pass in the cpal callback (scratch is preallocated for this).
/// How long a producer waits for the callback to apply a flush before doing it itself.
const FLUSH_WAIT: Duration = Duration::from_millis(50);
const SCRATCH_FRAMES: usize = 4096;
const PACKED_F32: ff::format::Sample = ff::format::Sample::F32(ff::format::sample::Type::Packed);

/// Single-producer single-consumer ring of f32 samples (interleaved stereo, even length).
pub struct Ring {
    buf: Box<[AtomicU32]>,
    write: AtomicUsize,
    read: AtomicUsize,
}

impl Ring {
    /// Ring holding `frames` stereo frames.
    pub fn new(frames: usize) -> Ring {
        let cap = frames.max(1) * 2;
        Ring {
            buf: (0..cap).map(|_| AtomicU32::new(0)).collect(),
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
        }
    }

    /// Producer: appends as many whole frames of `data` as fit; returns samples written.
    pub fn push(&self, data: &[f32]) -> usize {
        let cap = self.buf.len();
        let w = self.write.load(Ordering::Relaxed);
        let r = self.read.load(Ordering::Acquire);
        let n = (cap - (w - r)).min(data.len()) & !1;
        for (i, s) in data[..n].iter().enumerate() {
            self.buf[(w + i) % cap].store(s.to_bits(), Ordering::Relaxed);
        }
        self.write.store(w + n, Ordering::Release);
        n
    }

    /// Consumer: next stereo frame, if any.
    pub fn pop_frame(&self) -> Option<[f32; 2]> {
        let cap = self.buf.len();
        let r = self.read.load(Ordering::Relaxed);
        let w = self.write.load(Ordering::Acquire);
        if w - r < 2 {
            return None;
        }
        let l = f32::from_bits(self.buf[r % cap].load(Ordering::Relaxed));
        let rr = f32::from_bits(self.buf[(r + 1) % cap].load(Ordering::Relaxed));
        self.read.store(r + 2, Ordering::Release);
        Some([l, rr])
    }

    /// Producer: drops everything buffered while no consumer is running. `fetch_max` keeps a
    /// racing consumer from underflowing the ring (worst case a few stale frames survive).
    pub fn discard_from_producer(&self) {
        let w = self.write.load(Ordering::Acquire);
        self.read.fetch_max(w, Ordering::AcqRel);
    }

    /// Consumer: drops everything buffered.
    pub fn skip_all(&self) {
        self.skip_to(self.write.load(Ordering::Acquire));
    }

    /// Consumer: drops everything before write position `w`. Never moves `read` backwards
    /// (the producer may have discarded further meanwhile), so stale frames do not return.
    fn skip_to(&self, w: usize) {
        self.read.fetch_max(w, Ordering::AcqRel);
    }

    /// Buffered stereo frames.
    pub fn len_frames(&self) -> usize {
        let r = self.read.load(Ordering::Acquire);
        (self.write.load(Ordering::Acquire) - r) / 2
    }
}

/// One clip's audio stream into the mixer.
pub struct Voice {
    pub ring: Ring,
    volume: AtomicU32,
    pan: AtomicU32,
    audible: AtomicBool,
    /// Layer bus the voice sums into before the bus chain (0 = straight to the master).
    bus: AtomicU32,
    /// Effects of the clip, run on this voice alone.
    pub fx: FxChain,
    flush: AtomicBool,
    /// Whether the voice goes into the recording tap (the metronome click does not).
    recorded: AtomicBool,
}

impl Voice {
    pub fn new(ring_frames: usize) -> Voice {
        Voice {
            ring: Ring::new(ring_frames),
            volume: AtomicU32::new(1.0f32.to_bits()),
            pan: AtomicU32::new(0.0f32.to_bits()),
            audible: AtomicBool::new(false),
            bus: AtomicU32::new(0),
            fx: FxChain::new(1),
            flush: AtomicBool::new(false),
            recorded: AtomicBool::new(true),
        }
    }

    /// Excludes (or includes) the voice from the recording tap.
    pub fn set_recorded(&self, recorded: bool) {
        self.recorded.store(recorded, Ordering::Relaxed);
    }

    /// Linear volume (>= 0), pan -1 (left) .. 1 (right), and whether to play at all.
    pub fn set(&self, volume: f32, pan: f32, audible: bool) {
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
        self.pan.store(pan.to_bits(), Ordering::Relaxed);
        self.audible.store(audible, Ordering::Relaxed);
    }

    /// Routes the voice through layer bus `bus` (1-based, 0 = none).
    pub fn set_bus(&self, bus: u32) {
        self.bus.store(bus, Ordering::Relaxed);
    }

    fn audible(&self) -> bool {
        self.audible.load(Ordering::Relaxed)
    }

    /// True while a requested flush has not been applied by the consumer yet.
    pub fn flush_pending(&self) -> bool {
        self.flush.load(Ordering::Acquire)
    }

    /// Consumer: starts applying a requested flush. Returns the write position to drop up to.
    /// The flag stays set until `finish_flush`, so the producer cannot push fresh audio in
    /// between that the drop would eat (a slow callback may even outlast `FLUSH_WAIT`).
    fn begin_flush(&self) -> Option<usize> {
        if self.flush.load(Ordering::Acquire) {
            Some(self.ring.write.load(Ordering::Acquire))
        } else {
            None
        }
    }

    fn finish_flush(&self, w: usize) {
        self.ring.skip_to(w);
        self.flush.store(false, Ordering::Release);
    }

    /// Asks the consumer to drop what is buffered (used before a re-seek).
    pub fn request_flush(&self) {
        self.flush.store(true, Ordering::Release);
    }
}

/// Left/right gains for a pan in -1..1: a balance law, unity on the louder side and at
/// center. Non-finite pans count as center.
pub fn pan_gains(pan: f32) -> (f32, f32) {
    let p = if pan.is_finite() {
        pan.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    (1.0 - p.max(0.0), 1.0 + p.min(0.0))
}

/// Scratch and routing of one mix pass: preallocated so the callback never allocates.
pub struct MixFx {
    buses: Arc<Buses>,
    rate: u32,
    voice_buf: Vec<f32>,
    bus_buf: Vec<Vec<f32>>,
}

impl MixFx {
    /// Buffers for blocks of up to `samples` interleaved samples.
    pub fn new(buses: Arc<Buses>, rate: u32, samples: usize) -> MixFx {
        MixFx {
            buses,
            rate,
            voice_buf: vec![0.0; samples],
            bus_buf: vec![vec![0.0; samples]; MAX_BUSES],
        }
    }
}

/// Mixes every voice into `out` (interleaved stereo, overwritten) and clamps to [-1, 1].
/// Helper for tests and offline use: it allocates its scratch. The callback uses `mix_fx`.
pub fn mix_stereo(out: &mut [f32], voices: &[Arc<Voice>]) {
    let mut fx = MixFx::new(Arc::new(Buses::new()), 48_000, out.len());
    mix_voices(out, None, voices, &mut fx);
}

/// Like `mix_stereo`, and also writes into `tap` (same length) the mix of the voices that
/// are recorded: what the master recording hears, without the metronome.
pub fn mix_stereo_tap(out: &mut [f32], tap: &mut [f32], voices: &[Arc<Voice>]) {
    let mut fx = MixFx::new(Arc::new(Buses::new()), 48_000, out.len());
    mix_voices(out, Some(tap), voices, &mut fx);
}

/// Voice -> clip chain -> layer bus chain -> master chain. Allocation-free.
fn mix_voices(out: &mut [f32], mut tap: Option<&mut [f32]>, voices: &[Arc<Voice>], fx: &mut MixFx) {
    let len = out.len();
    out.fill(0.0);
    if let Some(t) = tap.as_deref_mut() {
        t.fill(0.0);
    }
    let mut used = [false; MAX_BUSES];
    for v in voices {
        if !v.audible() {
            v.ring.skip_all();
            continue;
        }
        if let Some(w) = v.begin_flush() {
            v.finish_flush(w);
        }
        let vol = f32::from_bits(v.volume.load(Ordering::Relaxed));
        let vol = if vol.is_finite() { vol.max(0.0) } else { 0.0 };
        let (gl, gr) = pan_gains(f32::from_bits(v.pan.load(Ordering::Relaxed)));
        let rec = v.recorded.load(Ordering::Relaxed);
        let vb = &mut fx.voice_buf[..len];
        vb.fill(0.0);
        for frame in vb.as_chunks_mut::<2>().0 {
            let Some([l, r]) = v.ring.pop_frame() else {
                break;
            };
            frame[0] = l * vol * gl;
            frame[1] = r * vol * gr;
        }
        v.fx.process(vb, 0, fx.rate);
        let bus = v.bus.load(Ordering::Relaxed) as usize;
        if (1..=MAX_BUSES).contains(&bus) {
            let b = &mut fx.bus_buf[bus - 1][..len];
            if !used[bus - 1] {
                b.fill(0.0);
                used[bus - 1] = true;
            }
            for (d, s) in b.iter_mut().zip(vb.iter()) {
                *d += *s;
            }
            continue;
        }
        for (d, s) in out.iter_mut().zip(vb.iter()) {
            *d += *s;
        }
        if rec && let Some(t) = tap.as_deref_mut() {
            for (d, s) in t.iter_mut().zip(vb.iter()) {
                *d += *s;
            }
        }
    }
    for (i, _) in used.iter().enumerate().filter(|(_, u)| **u) {
        let b = &mut fx.bus_buf[i][..len];
        fx.buses.layers[i].process(b, 0, fx.rate);
        for (d, s) in out.iter_mut().zip(b.iter()) {
            *d += *s;
        }
        if let Some(t) = tap.as_deref_mut() {
            for (d, s) in t.iter_mut().zip(b.iter()) {
                *d += *s;
            }
        }
    }
    fx.buses.master.process(out, 0, fx.rate);
    if let Some(t) = tap.as_deref_mut() {
        fx.buses.master.process(t, 1, fx.rate);
    }
    for s in out.iter_mut().chain(tap.into_iter().flatten()) {
        *s = s.clamp(-1.0, 1.0);
    }
}

/// Seconds of master audio the tap can hold before the recorder must drain it.
const TAP_SECONDS: usize = 2;
/// Highest device rate the tap sizes for.
const TAP_MAX_RATE: usize = 96_000;

/// Master-output tap for recording: the audio callback feeds the mixed stereo here while it
/// is on, the recorder thread drains it. One producer (the callback), one consumer.
pub struct Tap {
    ring: Ring,
    /// `(generation << 1) | on`: one word so stopping a stale generation is a single CAS.
    state: AtomicU64,
}

impl Tap {
    pub fn new() -> Tap {
        Tap {
            ring: Ring::new(TAP_SECONDS * TAP_MAX_RATE),
            state: AtomicU64::new(0),
        }
    }

    /// Starts collecting, from silence: stale samples are dropped. Returns the id of this
    /// recording; only that id can stop it (`stop_rec`) or read its samples (`pop_frame_rec`).
    pub fn start(&self) -> u64 {
        self.ring.skip_all();
        let prev = self
            .state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |s| {
                Some((((s >> 1) + 1) << 1) | 1)
            })
            .unwrap_or(0);
        (prev >> 1) + 1
    }

    pub fn stop(&self) {
        self.state.fetch_and(!1, Ordering::AcqRel);
    }

    /// Stops the tap only if recording `rec_id` is still the current one: a late worker of an
    /// old recording cannot cut a newer one.
    pub fn stop_rec(&self, rec_id: u64) {
        let on = (rec_id << 1) | 1;
        let _ = self
            .state
            .compare_exchange(on, rec_id << 1, Ordering::AcqRel, Ordering::Acquire);
    }

    pub fn is_on(&self) -> bool {
        self.state.load(Ordering::Acquire) & 1 == 1
    }

    /// Like `pop_frame`, but nothing once a newer recording than `rec_id` has started.
    pub fn pop_frame_rec(&self, rec_id: u64) -> Option<[f32; 2]> {
        if self.state.load(Ordering::Acquire) >> 1 != rec_id {
            return None;
        }
        self.ring.pop_frame()
    }

    /// Producer (audio callback): appends interleaved stereo while the tap is on. Samples
    /// that do not fit are dropped, never blocking. Allocation-free.
    pub fn feed(&self, stereo: &[f32]) {
        if self.is_on() {
            self.ring.push(stereo);
        }
    }

    /// Consumer: next stereo frame.
    pub fn pop_frame(&self) -> Option<[f32; 2]> {
        self.ring.pop_frame()
    }

    /// Buffered stereo frames.
    pub fn len_frames(&self) -> usize {
        self.ring.len_frames()
    }
}

impl Default for Tap {
    fn default() -> Self {
        Self::new()
    }
}

/// Producer side of a re-seek: appends `buf[off..]` unless a flush is still pending. The
/// consumer applies the flush by dropping everything in the ring, so data pushed before it
/// does would be thrown away with the stale audio. None = wait and retry.
/// If the flush is still pending after `waited` >= `FLUSH_WAIT` the output stream is not
/// running (no callback to apply it), so the producer applies the flush itself.
pub fn push_after_flush(voice: &Voice, buf: &[f32], off: usize, waited: Duration) -> Option<usize> {
    if voice.flush_pending() {
        if waited < FLUSH_WAIT {
            return None;
        }
        voice.ring.discard_from_producer();
        voice.flush.store(false, Ordering::Release);
    }
    Some(voice.ring.push(&buf[off..]))
}

type VoiceList = Arc<Mutex<Vec<Arc<Voice>>>>;

struct Guard {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// The output device stream and its voices. Cheap to clone; the stream closes when the last
/// clone is dropped.
#[derive(Clone)]
pub struct AudioEngine {
    voices: VoiceList,
    tap: Arc<Tap>,
    buses: Arc<Buses>,
    rate: Arc<AtomicU32>,
    /// What the stream is (re)opened with; `reconfigure` changes it and raises `failed`.
    cfg: Arc<Mutex<OutConfig>>,
    failed: Arc<AtomicBool>,
    /// Why the last reconfigure could not open (the old settings were reopened instead).
    reopen_error: Arc<Mutex<Option<String>>>,
    _guard: Arc<Guard>,
}

/// Output stream wishes. `None` fields use what the device offers by default; a wish the
/// device cannot satisfy falls back to the nearest supported value (see `choose`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct OutConfig {
    /// Device name; the default device when None or not found.
    pub device: Option<String>,
    pub sample_rate: Option<u32>,
    /// Buffer size in frames.
    pub buffer_frames: Option<u32>,
    pub channels: Option<u16>,
}

/// One supported (channels, rate range) of a device: the part of a cpal config range that
/// `choose` looks at, so the choice can be tested without a device.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub channels: u16,
    pub min_rate: u32,
    pub max_rate: u32,
}

/// (channels, rate) to open: the wishes when a range holds them, else the closest the
/// device supports. `default` is the device default, used for any wish left open.
pub fn choose(ranges: &[Range], default: (u16, u32), want: &OutConfig) -> (u16, u32) {
    let ch = want.channels.unwrap_or(default.0);
    let rate = want.sample_rate.unwrap_or(default.1);
    let clamp_in = |r: &Range| rate.clamp(r.min_rate, r.max_rate);
    // The wished channel count, with the rate clamped into the nearest range for it.
    let nearest = |ch: u16| {
        ranges
            .iter()
            .filter(|r| r.channels == ch)
            .map(clamp_in)
            .min_by_key(|c| c.abs_diff(rate))
    };
    if let Some(r) = nearest(ch) {
        return (ch, r);
    }
    // No such channel count: keep the device's, still honouring the rate.
    match nearest(default.0) {
        Some(r) => (default.0, r),
        None => default,
    }
}

/// The buffer size to request: the wish clamped into the supported range, or None (host
/// default) when nothing was asked or the device does not say what it supports.
pub fn buffer_choice(want: Option<u32>, supported: Option<(u32, u32)>) -> Option<u32> {
    let (min, max) = supported?;
    want.map(|w| w.clamp(min, max))
}

impl AudioEngine {
    /// Opens the default output device. Needs a real audio device (not covered by CI tests).
    pub fn start() -> Result<AudioEngine, String> {
        Self::start_on(None)
    }

    /// Opens the output device called `device` (the default one if None or not found).
    pub fn start_on(device: Option<&str>) -> Result<AudioEngine, String> {
        Self::start_with(OutConfig {
            device: device.map(str::to_owned),
            ..Default::default()
        })
    }

    /// Opens the output with the wishes in `cfg`.
    pub fn start_with(cfg: OutConfig) -> Result<AudioEngine, String> {
        let cfg = Arc::new(Mutex::new(cfg));
        let cfg_thread = cfg.clone();
        let good = Arc::new(Mutex::new(cfg.lock().unwrap().clone()));
        let reopen_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let error_thread = reopen_error.clone();
        let failed = Arc::new(AtomicBool::new(false));
        let failed_thread = failed.clone();
        let voices: VoiceList = Arc::new(Mutex::new(Vec::new()));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let v = voices.clone();
        let tap = Arc::new(Tap::new());
        let t = tap.clone();
        let buses = Arc::new(Buses::new());
        let bs = buses.clone();
        let shared_rate = Arc::new(AtomicU32::new(0));
        let sr = shared_rate.clone();
        let thread = std::thread::Builder::new()
            .name("vjc-audio-out".into())
            .spawn(move || {
                // The cpal stream is not Send: it lives and dies on this thread.
                let failed = failed_thread;
                let open = || {
                    open_or_fall_back(&cfg_thread, &good, &error_thread, |want| {
                        open_stream(v.clone(), t.clone(), bs.clone(), failed.clone(), want)
                    })
                };
                match open() {
                    Ok((stream, rate)) => {
                        sr.store(rate, Ordering::Release);
                        let _ = ready_tx.send(Ok(rate));
                        // Returns when the engine is dropped; reopens the device if it dies.
                        supervise(stream, open, &sr, &failed, &stop_rx, REOPEN, REOPEN_POLL);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        ready_rx
            .recv()
            .map_err(|_| "audio thread ended unexpectedly".to_string())??;
        Ok(AudioEngine {
            voices,
            tap,
            buses,
            rate: shared_rate,
            cfg,
            failed,
            reopen_error,
            _guard: Arc::new(Guard {
                stop: Some(stop_tx),
                thread: Some(thread),
            }),
        })
    }

    /// Reopens the output with new wishes while running: voices, bus effects and the
    /// recording tap stay, decoders follow the new rate. False when nothing changed.
    pub fn reconfigure(&self, cfg: OutConfig) -> bool {
        swap_cfg(&self.cfg, &self.failed, cfg)
    }

    /// The reason the last reconfigure failed and the previous settings were reopened.
    pub fn take_reopen_error(&self) -> Option<String> {
        self.reopen_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// The recording tap of the master output.
    pub fn tap(&self) -> Arc<Tap> {
        self.tap.clone()
    }

    /// Layer bus and master effect chains of the mixer.
    pub fn buses(&self) -> Arc<Buses> {
        self.buses.clone()
    }

    /// Device sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.rate.load(Ordering::Acquire)
    }

    fn add_voice(&self) -> Arc<Voice> {
        let v = Arc::new(Voice::new(
            (self.sample_rate() as f64 * RING_SECONDS) as usize,
        ));
        self.voices.lock().unwrap().push(v.clone());
        v
    }

    fn remove_voice(&self, v: &Arc<Voice>) {
        self.voices.lock().unwrap().retain(|x| !Arc::ptr_eq(x, v));
    }
}

/// Beats per bar for the metronome accent (the beat lights in the UI use the same bar).
pub const BEATS_PER_BAR: i64 = 4;
/// Click length in seconds.
const CLICK_SECONDS: f64 = 0.03;
/// Click pitch: the downbeat is higher so the bar is audible.
const CLICK_HZ: f64 = 1000.0;
const CLICK_ACCENT_HZ: f64 = 1600.0;
const CLICK_GAIN: f32 = 0.5;

/// A short synthesized click (interleaved stereo f32): a sine with a 1 ms linear attack and
/// an exponential decay to near silence, so it starts and ends at 0 without a pop.
pub fn click_samples(rate: u32, accent: bool) -> Vec<f32> {
    let n = (rate as f64 * CLICK_SECONDS) as usize;
    let hz = if accent { CLICK_ACCENT_HZ } else { CLICK_HZ };
    let attack = (rate as f64 * 0.001).max(1.0);
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t = i as f64 / rate as f64;
        let env = (i as f64 / attack).min(1.0) * (-t / (CLICK_SECONDS / 6.0)).exp();
        let v = ((2.0 * std::f64::consts::PI * hz * t).sin() * env) as f32 * CLICK_GAIN;
        out.push(v);
        out.push(v);
    }
    // Hard zero on the last frame so the click never ends mid-wave.
    if let Some(last) = out.last_chunk_mut::<2>() {
        *last = [0.0, 0.0];
    }
    out
}

/// Which click a clock tick asks for: `beat` is the beat that just started (None = no beat
/// boundary this tick). Returns `Some(accent)`, accent on the downbeat of each bar.
pub fn beat_click(enabled: bool, beat: Option<i64>) -> Option<bool> {
    let b = beat?;
    enabled.then(|| b.rem_euclid(BEATS_PER_BAR) == 0)
}

/// Metronome output: its own voice in the mixer, fed one click at a time.
pub struct Metronome {
    engine: AudioEngine,
    voice: Arc<Voice>,
}

impl Metronome {
    pub fn new(engine: &AudioEngine) -> Metronome {
        let voice = engine.add_voice();
        voice.set(1.0, 0.0, true);
        // A monitoring click, not program audio.
        voice.set_recorded(false);
        Metronome {
            engine: engine.clone(),
            voice,
        }
    }

    /// Queues a click now (dropped if the previous one has not played out, never blocks).
    pub fn click(&self, accent: bool) {
        let samples = click_samples(self.engine.sample_rate().max(8000), accent);
        self.voice.ring.push(&samples);
    }
}

impl Drop for Metronome {
    fn drop(&mut self) {
        self.engine.remove_voice(&self.voice);
    }
}

/// Stores `cfg` and raises `failed` (the supervisor then reopens) when it differs.
fn swap_cfg(slot: &Mutex<OutConfig>, failed: &AtomicBool, cfg: OutConfig) -> bool {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    if *g == cfg {
        return false;
    }
    *g = cfg;
    failed.store(true, Ordering::Relaxed);
    true
}

/// Opens with the wishes in `slot`. When that fails and a different configuration worked
/// before, the slot goes back to it, the reason is kept in `error` and it is opened instead,
/// so a bad reconfigure never leaves the app without sound.
fn open_or_fall_back<T>(
    slot: &Mutex<OutConfig>,
    good: &Mutex<OutConfig>,
    error: &Mutex<Option<String>>,
    mut try_open: impl FnMut(&OutConfig) -> Result<T, String>,
) -> Result<T, String> {
    let want = slot.lock().unwrap_or_else(|e| e.into_inner()).clone();
    match try_open(&want) {
        Ok(t) => {
            *good.lock().unwrap_or_else(|e| e.into_inner()) = want;
            Ok(t)
        }
        Err(e) => {
            let prev = good.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if prev == want {
                return Err(e);
            }
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = prev.clone();
            *error.lock().unwrap_or_else(|e| e.into_inner()) = Some(e);
            try_open(&prev)
        }
    }
}

/// Pause between attempts to reopen a lost output device.
const REOPEN: Duration = Duration::from_secs(2);
/// How often the supervisor looks at the stream's error flag.
const REOPEN_POLL: Duration = Duration::from_millis(100);

/// Keeps `stream` alive until `stop` closes. When `failed` is raised (the device was
/// unplugged or changed) the stream is dropped and `open` is retried every `retry` until it
/// works. `S` is a cpal stream in the app and a fake in tests.
fn supervise<S>(
    stream: S,
    mut open: impl FnMut() -> Result<(S, u32), String>,
    rate: &AtomicU32,
    failed: &AtomicBool,
    stop: &mpsc::Receiver<()>,
    retry: Duration,
    poll: Duration,
) {
    let mut stream = Some(stream);
    loop {
        let wait = if stream.is_some() { poll } else { retry };
        if !matches!(
            stop.recv_timeout(wait),
            Err(mpsc::RecvTimeoutError::Timeout)
        ) {
            return;
        }
        if stream.is_some() && failed.swap(false, Ordering::Relaxed) {
            eprintln!("vjc-media: audio device lost, reopening");
            stream = None;
        }
        if stream.is_none()
            && let Ok((s, new_rate)) = open()
        {
            // The new device may run at another rate: decoders follow this value.
            rate.store(new_rate, Ordering::Release);
            failed.store(false, Ordering::Relaxed);
            stream = Some(s);
        }
    }
}

/// Names of the audio output devices.
pub fn output_devices() -> Vec<String> {
    let Ok(devs) = cpal::default_host().output_devices() else {
        return vec![];
    };
    devs.filter_map(|d| d.description().ok().map(|d| d.name().to_owned()))
        .collect()
}

/// The output device named `name`, else the default one.
fn pick_output(name: Option<&str>) -> Option<cpal::Device> {
    let host = cpal::default_host();
    name.and_then(|n| {
        host.output_devices()
            .ok()?
            .find(|d| d.description().map(|d| d.name() == n).unwrap_or(false))
    })
    .or_else(|| host.default_output_device())
}

fn open_stream(
    voices: VoiceList,
    tap: Arc<Tap>,
    buses: Arc<Buses>,
    failed: Arc<AtomicBool>,
    want: &OutConfig,
) -> Result<(cpal::Stream, u32), String> {
    let device = pick_output(want.device.as_deref()).ok_or("No audio output device")?;
    let default = device
        .default_output_config()
        .map_err(|e| format!("Audio output config: {e}"))?;
    let mut ranges: Vec<cpal::SupportedStreamConfigRange> = device
        .supported_output_configs()
        .map(|r| r.collect())
        .unwrap_or_default();
    // The device's own sample formats first: F32 where offered.
    ranges.sort_by_key(|r| r.sample_format() != cpal::SampleFormat::F32);
    let plain: Vec<Range> = ranges
        .iter()
        .map(|r| Range {
            channels: r.channels(),
            min_rate: r.min_sample_rate(),
            max_rate: r.max_sample_rate(),
        })
        .collect();
    let (ch, rate) = choose(&plain, (default.channels(), default.sample_rate()), want);
    let supported = ranges
        .into_iter()
        .find(|r| r.channels() == ch && r.contains_rate(rate))
        .and_then(|r| r.try_with_sample_rate(rate))
        .unwrap_or(default);
    let mut cfg = supported.config();
    let range = match supported.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => Some((*min, *max)),
        cpal::SupportedBufferSize::Unknown => None,
    };
    if let Some(n) = buffer_choice(want.buffer_frames, range) {
        cfg.buffer_size = cpal::BufferSize::Fixed(n);
    }
    let rate = cfg.sample_rate;
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, cfg, voices, tap, buses, failed),
        cpal::SampleFormat::I16 => build::<i16>(&device, cfg, voices, tap, buses, failed),
        cpal::SampleFormat::U16 => build::<u16>(&device, cfg, voices, tap, buses, failed),
        cpal::SampleFormat::I32 => build::<i32>(&device, cfg, voices, tap, buses, failed),
        other => return Err(format!("Unsupported audio sample format {other}")),
    }
    .map_err(|e| format!("Audio output: {e}"))?;
    stream.play().map_err(|e| format!("Audio output: {e}"))?;
    Ok((stream, rate))
}

/// Mixes one callback block into `st`. If the voice list is busy the block is silence, and
/// the tap still gets what was output: it must receive every frame so its sample count (the
/// recording's audio PTS) never drifts from the video.
fn render_block(voices: &VoiceList, tap: &Tap, fx: &mut MixFx, st: &mut [f32], tp: &mut [f32]) {
    match voices.try_lock() {
        Ok(g) if tap.is_on() => {
            mix_voices(st, Some(tp), &g, fx);
            tap.feed(tp);
        }
        Ok(g) => mix_voices(st, None, &g, fx),
        Err(_) => {
            st.fill(0.0);
            tap.feed(st);
        }
    }
}

fn build<T>(
    device: &cpal::Device,
    cfg: cpal::StreamConfig,
    voices: VoiceList,
    tap: Arc<Tap>,
    buses: Arc<Buses>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let ch = (cfg.channels as usize).max(1);
    let mut scratch = vec![0f32; SCRATCH_FRAMES * 2];
    let mut tap_scratch = vec![0f32; SCRATCH_FRAMES * 2];
    let mut mixfx = MixFx::new(buses, cfg.sample_rate, SCRATCH_FRAMES * 2);
    device.build_output_stream(
        cfg,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for chunk in data.chunks_mut(SCRATCH_FRAMES * ch) {
                let frames = chunk.len() / ch;
                let st = &mut scratch[..frames * 2];
                let tp = &mut tap_scratch[..frames * 2];
                render_block(&voices, &tap, &mut mixfx, st, tp);
                for (i, f) in chunk.chunks_exact_mut(ch).enumerate() {
                    let (l, r) = (st[i * 2], st[i * 2 + 1]);
                    if ch == 1 {
                        f[0] = T::from_sample((l + r) * 0.5);
                    } else {
                        f[0] = T::from_sample(l);
                        f[1] = T::from_sample(r);
                        f[2..].fill(T::from_sample(0.0f32));
                    }
                }
            }
        },
        move |e| {
            eprintln!("vjc-media: audio stream error: {e}");
            failed.store(true, Ordering::Relaxed);
        },
        None,
    )
}

/// Audio stream of a media file, decoded and resampled to interleaved stereo f32.
pub struct AudioDecoder {
    ictx: ff::format::context::Input,
    decoder: ff::decoder::Audio,
    stream_idx: usize,
    tb: f64,
    out_rate: u32,
    resampler: Option<ff::software::resampling::Context>,
    pos: f64,
    eof: bool,
}

impl AudioDecoder {
    /// Opens `path`; `Ok(None)` when the file has no audio stream.
    pub fn open(path: &Path, out_rate: u32) -> Result<Option<AudioDecoder>, ff::Error> {
        ff::init()?;
        let ictx = ff::format::input(&path)?;
        let (stream_idx, tb, params) = {
            let Some(st) = ictx.streams().best(ff::media::Type::Audio) else {
                return Ok(None);
            };
            (st.index(), f64::from(st.time_base()), st.parameters())
        };
        let decoder = ff::codec::context::Context::from_parameters(params)?
            .decoder()
            .audio()?;
        Ok(Some(AudioDecoder {
            ictx,
            decoder,
            stream_idx,
            tb,
            out_rate,
            resampler: None,
            pos: 0.0,
            eof: false,
        }))
    }

    /// Output rate currently produced.
    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    /// Switches the output rate (device changed). Rebuilds the resampler; the caller must
    /// flush and re-seek to the playhead.
    pub fn set_out_rate(&mut self, rate: u32) {
        if rate != self.out_rate {
            self.out_rate = rate;
            self.resampler = None;
        }
    }

    /// Media time (seconds) after the last chunk returned by `read_chunk`.
    pub fn pos(&self) -> f64 {
        self.pos
    }

    pub fn at_end(&self) -> bool {
        self.eof
    }

    /// Seeks near `t` seconds (to the previous keyframe).
    pub fn seek(&mut self, t: f64) -> Result<(), ff::Error> {
        let ts = (t.max(0.0) * f64::from(ff::ffi::AV_TIME_BASE)) as i64;
        self.ictx.seek(ts, ..ts)?;
        self.decoder.flush();
        self.resampler = None;
        self.eof = false;
        self.pos = t.max(0.0);
        Ok(())
    }

    /// Appends the next decoded chunk to `out` (interleaved stereo at the output rate).
    /// Returns the frames appended; 0 means end of stream. Bad packets are skipped.
    pub fn read_chunk(&mut self, out: &mut Vec<f32>) -> Result<usize, ff::Error> {
        let mut decoded = ff::frame::Audio::empty();
        loop {
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                let n = self.resample(&decoded, out)?;
                if n > 0 {
                    return Ok(n);
                }
                continue;
            }
            if self.eof {
                return self.drain_resampler(out);
            }
            let mut pkt = ff::Packet::empty();
            match pkt.read(&mut self.ictx) {
                Ok(()) => {
                    if pkt.stream() == self.stream_idx
                        && let Err(e) = self.decoder.send_packet(&pkt)
                    {
                        eprintln!("vjc-media: audio packet skipped: {e}");
                    }
                }
                Err(_) => {
                    self.eof = true;
                    let _ = self.decoder.send_eof();
                }
            }
        }
    }

    /// End of stream: hands out what the resampler still holds (once).
    fn drain_resampler(&mut self, out: &mut Vec<f32>) -> Result<usize, ff::Error> {
        let Some(r) = self.resampler.take() else {
            return Ok(0);
        };
        let mut r = r;
        let mut res = ff::frame::Audio::new(PACKED_F32, 8192, ff::ChannelLayout::STEREO);
        r.flush(&mut res)?;
        let n = res.samples();
        out.extend_from_slice(&res.plane::<f32>(0)[..n * 2]);
        Ok(n)
    }

    fn resample(
        &mut self,
        decoded: &ff::frame::Audio,
        out: &mut Vec<f32>,
    ) -> Result<usize, ff::Error> {
        let mut src = decoded.clone();
        if src.channel_layout().is_empty() {
            src.set_channel_layout(ff::ChannelLayout::default(i32::from(src.channels())));
        }
        if self.resampler.is_none() {
            self.resampler = Some(ff::software::resampling::Context::get(
                src.format(),
                src.channel_layout(),
                src.rate(),
                PACKED_F32,
                ff::ChannelLayout::STEREO,
                self.out_rate,
            )?);
        }
        if let Some(pts) = decoded.timestamp() {
            self.pos = pts as f64 * self.tb;
        }
        // The output frame must be big enough for the rate change, or swr keeps the excess.
        let cap = (src.samples() as u64 * u64::from(self.out_rate) / u64::from(src.rate().max(1)))
            as usize
            + 256;
        let mut res = ff::frame::Audio::new(PACKED_F32, cap, ff::ChannelLayout::STEREO);
        self.resampler.as_mut().unwrap().run(&src, &mut res)?;
        let n = res.samples();
        if n > 0 {
            out.extend_from_slice(&res.plane::<f32>(0)[..n * 2]);
        }
        self.pos += decoded.samples() as f64 / f64::from(decoded.rate().max(1));
        Ok(n)
    }
}

/// True when the decoder (`dec`, media seconds after the last chunk) is too far from the
/// wanted playhead time `want`: behind it, or further ahead than the ring can explain.
pub fn needs_resync(dec: f64, want: f64) -> bool {
    let lead = dec - want;
    !(-0.2..=TARGET_LEAD + 0.4).contains(&lead)
}

/// Makes `dec` follow the device rate; true when it changed.
pub fn follow_rate(dec: &mut AudioDecoder, rate: &AtomicU32) -> bool {
    let r = rate.load(Ordering::Acquire);
    if r == 0 || r == dec.out_rate() {
        return false;
    }
    dec.set_out_rate(r);
    true
}

/// Audio state owned by a `Player`: voice, decode thread, engine handle.
pub struct AudioPart {
    pub(crate) voice: Arc<Voice>,
    engine: AudioEngine,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl AudioPart {
    pub(crate) fn start(
        engine: &AudioEngine,
        path: &Path,
        shared: Arc<Shared>,
    ) -> Option<AudioPart> {
        let voice = engine.add_voice();
        let stop = Arc::new(AtomicBool::new(false));
        let (v, s, p, rate) = (
            voice.clone(),
            stop.clone(),
            path.to_path_buf(),
            engine.rate.clone(),
        );
        let thread = std::thread::Builder::new()
            .name("vjc-audio-dec".into())
            .spawn(move || {
                // A decoder panic silences this clip's audio, nothing else.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_audio(&p, &rate, &v, &shared, &s)
                }));
            })
            .ok();
        if thread.is_none() {
            engine.remove_voice(&voice);
            return None;
        }
        Some(AudioPart {
            voice,
            engine: engine.clone(),
            stop,
            thread,
        })
    }

    pub(crate) fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.engine.remove_voice(&self.voice);
    }
}

impl Drop for AudioPart {
    fn drop(&mut self) {
        // `stop()` consumes self and ends up here too; this covers a plain drop.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.engine.remove_voice(&self.voice);
    }
}

fn run_audio(path: &Path, rate: &AtomicU32, voice: &Voice, shared: &Shared, stop: &AtomicBool) {
    let mut dec = match AudioDecoder::open(path, rate.load(Ordering::Acquire)) {
        Ok(Some(d)) => d,
        Ok(None) => return,
        Err(e) => {
            eprintln!("vjc-media: audio of {} unavailable: {e}", path.display());
            return;
        }
    };
    let nap = |ms| std::thread::sleep(Duration::from_millis(ms));
    let mut buf: Vec<f32> = Vec::new();
    let mut off = 0usize;
    let mut synced = false;
    let mut flush_since: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) && !shared.stop.load(Ordering::Relaxed) {
        if follow_rate(&mut dec, rate) {
            // Device rate changed: old-rate audio is wrong, drop it and resync.
            voice.request_flush();
            flush_since = Some(Instant::now());
            buf.clear();
            off = 0;
            synced = false;
        }
        let target = (dec.out_rate() as f64 * TARGET_LEAD) as usize;
        let dur = f64::from_bits(shared.duration.load(Ordering::Relaxed));
        if !voice.audible() || dur <= 0.0 {
            voice.request_flush();
            synced = false;
            nap(10);
            continue;
        }
        let want = f64::from_bits(shared.position.load(Ordering::Relaxed)) * dur;
        // At the end of a shorter audio track, wait for the playhead (or a loop) instead of
        // seeking again and again.
        let waiting_at_end = dec.at_end() && off >= buf.len() && want >= dec.pos() - 0.2;
        if waiting_at_end {
            nap(20);
            continue;
        }
        if !synced || needs_resync(dec.pos(), want) {
            if let Err(e) = dec.seek(want) {
                eprintln!("vjc-media: audio seek failed: {e}");
                nap(50);
                continue;
            }
            voice.request_flush();
            flush_since = Some(Instant::now());
            buf.clear();
            off = 0;
            synced = true;
        }
        if off < buf.len() {
            let waited = flush_since.map_or(FLUSH_WAIT, |t| t.elapsed());
            let Some(n) = push_after_flush(voice, &buf, off, waited) else {
                nap(2);
                continue;
            };
            off += n;
            if off < buf.len() {
                nap(5);
                continue;
            }
            buf.clear();
            off = 0;
        }
        if voice.ring.len_frames() >= target {
            nap(5);
            continue;
        }
        match dec.read_chunk(&mut buf) {
            Ok(_) => {}
            Err(e) => {
                eprintln!("vjc-media: audio decode error: {e}");
                nap(50);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn click_is_short_bounded_and_accent_is_higher() {
        let (n, a) = (click_samples(48000, false), click_samples(48000, true));
        assert_eq!(n.len(), 48000 * 30 / 1000 * 2);
        assert!(n.iter().all(|s| s.abs() <= 0.5 + 1e-6));
        assert!(n.iter().any(|s| s.abs() > 0.1), "audible");
        assert_eq!(n[..2], [0.0, 0.0], "starts at zero");
        assert_eq!(n[n.len() - 2..], [0.0, 0.0], "ends at zero");
        assert_eq!(n[0], n[1], "centered");
        // Higher pitch = more zero crossings.
        let zc = |v: &[f32]| v.windows(4).filter(|w| w[0] * w[2] < 0.0).count();
        assert!(zc(&a) > zc(&n));
        // Decays: the tail is much quieter than the head.
        let peak = |v: &[f32]| v.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak(&n[n.len() / 2..]) < peak(&n) * 0.3);
    }

    #[test]
    fn beat_click_accents_the_downbeat_only() {
        let got: Vec<_> = (0..8).map(|b| beat_click(true, Some(b))).collect();
        let want: Vec<_> = (0..8).map(|b| Some(b % 4 == 0)).collect();
        assert_eq!(got, want);
        assert_eq!(beat_click(false, Some(0)), None, "off = silent");
        assert_eq!(beat_click(true, None), None, "no beat boundary = no click");
        assert_eq!(beat_click(true, Some(-4)), Some(true));
    }

    use super::*;
    use std::process::Command;

    fn voice_with(samples: &[f32], vol: f32, pan: f32) -> Arc<Voice> {
        let v = Arc::new(Voice::new(64));
        v.set(vol, pan, true);
        assert_eq!(v.ring.push(samples), samples.len());
        v
    }

    #[test]
    fn pan_law() {
        assert_eq!(pan_gains(0.0), (1.0, 1.0));
        assert_eq!(pan_gains(-1.0), (1.0, 0.0));
        assert_eq!(pan_gains(1.0), (0.0, 1.0));
        assert_eq!(pan_gains(0.5), (0.5, 1.0));
        assert_eq!(pan_gains(9.0), (0.0, 1.0));
        assert_eq!(pan_gains(f32::NAN), (1.0, 1.0));
    }

    #[test]
    fn mixer_applies_volume_and_pan() {
        let v = voice_with(&[0.5, 0.5, 0.5, 0.5], 0.5, 1.0);
        let mut out = [9.0f32; 4];
        mix_stereo(&mut out, &[v]);
        assert_eq!(out, [0.0, 0.25, 0.0, 0.25]);
    }

    #[test]
    fn tap_hears_the_master_mix_without_the_metronome_voice() {
        let clip = voice_with(&[0.25, 0.25, 0.25, 0.25], 1.0, 0.0);
        let click = voice_with(&[0.5, 0.5, 0.5, 0.5], 1.0, 0.0);
        click.set_recorded(false);
        let (mut out, mut tap) = ([0.0f32; 4], [9.0f32; 4]);
        mix_stereo_tap(&mut out, &mut tap, &[clip, click]);
        assert_eq!(out, [0.75; 4], "speakers hear both");
        assert_eq!(tap, [0.25; 4], "the recording only the program audio");
        // The tap follows the clamp and the voice volume like the speakers do.
        let loud = voice_with(&[0.9, 0.9, 0.9, 0.9], 2.0, 0.0);
        mix_stereo_tap(&mut out, &mut tap, &[loud]);
        assert_eq!((out, tap), ([1.0; 4], [1.0; 4]));
    }

    #[test]
    fn tap_collects_only_while_on_and_starts_from_silence() {
        let t = Tap::new();
        t.feed(&[0.1, 0.1, 0.2, 0.2]);
        assert_eq!(t.len_frames(), 0, "off: nothing buffered");
        t.start();
        t.feed(&[0.1, 0.1, 0.2, 0.2]);
        assert_eq!(t.pop_frame(), Some([0.1, 0.1]));
        t.stop();
        t.feed(&[0.3, 0.3]);
        assert_eq!(t.len_frames(), 1, "only the unread frame from before");
        t.start();
        assert_eq!(t.len_frames(), 0, "stale audio is dropped on start");
    }

    #[test]
    fn mixer_sums_voices_and_clamps() {
        let a = voice_with(&[0.25, -0.25], 1.0, 0.0);
        let b = voice_with(&[0.25, -0.25], 1.0, 0.0);
        let mut out = [0.0f32; 2];
        mix_stereo(&mut out, &[a, b]);
        assert_eq!(out, [0.5, -0.5]);
        let c = voice_with(&[0.9, -0.9], 1.0, 0.0);
        let d = voice_with(&[0.9, -0.9], 1.0, 0.0);
        mix_stereo(&mut out, &[c, d]);
        assert_eq!(out, [1.0, -1.0]);
    }

    #[test]
    fn mixer_silence_when_muted_empty_or_flushed() {
        let v = voice_with(&[0.5, 0.5], 1.0, 0.0);
        v.set(1.0, 0.0, false);
        let mut out = [1.0f32; 2];
        mix_stereo(&mut out, std::slice::from_ref(&v));
        assert_eq!(out, [0.0, 0.0]);
        assert_eq!(v.ring.len_frames(), 0); // muted voices are drained
        v.set(1.0, 0.0, true);
        mix_stereo(&mut out, std::slice::from_ref(&v));
        assert_eq!(out, [0.0, 0.0]); // underrun
        v.ring.push(&[0.5, 0.5]);
        v.request_flush();
        mix_stereo(&mut out, std::slice::from_ref(&v));
        assert_eq!(out, [0.0, 0.0]);
        v.set(f32::NAN, 0.0, true);
        v.ring.push(&[0.5, 0.5]);
        mix_stereo(&mut out, &[v]);
        assert_eq!(out, [0.0, 0.0]); // NaN volume is silence
    }

    #[test]
    fn ring_wraps_and_never_overfills() {
        let r = Ring::new(2); // 4 samples
        assert_eq!(r.push(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]), 4);
        assert_eq!(r.pop_frame(), Some([1.0, 2.0]));
        assert_eq!(r.push(&[7.0, 8.0]), 2);
        assert_eq!(r.pop_frame(), Some([3.0, 4.0]));
        assert_eq!(r.pop_frame(), Some([7.0, 8.0]));
        assert_eq!(r.pop_frame(), None);
    }

    #[test]
    fn push_waits_for_the_pending_flush_so_fresh_audio_survives() {
        let v = Arc::new(Voice::new(64));
        v.set(1.0, 0.0, true);
        v.ring.push(&[0.9, 0.9]); // stale audio from before the seek
        v.request_flush();
        // The decoder has new audio but must not push until the flush is applied.
        let fresh = [0.5f32, 0.5];
        assert_eq!(push_after_flush(&v, &fresh, 0, Duration::ZERO), None);
        let mut out = [1.0f32; 2];
        mix_stereo(&mut out, std::slice::from_ref(&v)); // consumer applies the flush
        assert_eq!(out, [0.0, 0.0]);
        assert_eq!(push_after_flush(&v, &fresh, 0, Duration::ZERO), Some(2));
        mix_stereo(&mut out, std::slice::from_ref(&v));
        assert_eq!(out, [0.5, 0.5]); // not eaten by a late flush
    }

    #[test]
    fn flush_in_progress_keeps_producer_waiting() {
        let v = Voice::new(64);
        v.ring.push(&[0.9, 0.9]);
        v.request_flush();
        let w = v.begin_flush().expect("pending");
        // Callback is mid-flush: the producer must not push yet.
        assert_eq!(push_after_flush(&v, &[0.5, 0.5], 0, Duration::ZERO), None);
        v.finish_flush(w);
        assert_eq!(
            push_after_flush(&v, &[0.5, 0.5], 0, Duration::ZERO),
            Some(2)
        );
        assert_eq!(v.ring.pop_frame(), Some([0.5, 0.5]));
    }

    #[test]
    fn consumer_skip_never_moves_read_backwards() {
        let r = Ring::new(8);
        r.push(&[1.0; 8]);
        r.discard_from_producer(); // read = 8
        r.skip_to(2); // stale snapshot
        r.push(&[0.5, 0.5]);
        assert_eq!(r.pop_frame(), Some([0.5, 0.5]));
    }

    #[test]
    fn supervisor_reopens_a_lost_device_with_retry() {
        use std::sync::atomic::AtomicUsize;
        let failed = Arc::new(AtomicBool::new(false));
        let opens = Arc::new(AtomicUsize::new(0));
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (f2, o2) = (failed.clone(), opens.clone());
        let h = std::thread::spawn(move || {
            supervise(
                0u32,
                || {
                    // First reopen attempt fails, the next ones work.
                    if o2.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err("no device".into())
                    } else {
                        Ok((1u32, 48000))
                    }
                },
                &AtomicU32::new(0),
                &f2,
                &stop_rx,
                Duration::from_millis(10),
                Duration::from_millis(5),
            )
        });
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(opens.load(Ordering::SeqCst), 0); // healthy: nothing reopened
        failed.store(true, Ordering::Relaxed); // the cpal error callback
        for _ in 0..200 {
            if opens.load(Ordering::SeqCst) >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(opens.load(Ordering::SeqCst), 2); // failed once, retried, opened
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(opens.load(Ordering::SeqCst), 2); // and stays open
        drop(stop_tx);
        h.join().unwrap();
    }

    #[test]
    fn choose_honours_wishes_and_falls_back_to_the_nearest_supported() {
        let r = |channels, min_rate, max_rate| Range {
            channels,
            min_rate,
            max_rate,
        };
        let ranges = [r(2, 44_100, 96_000), r(8, 48_000, 48_000)];
        let want = |sample_rate, channels| OutConfig {
            sample_rate,
            channels,
            ..Default::default()
        };
        let d = (2, 48_000);
        assert_eq!(
            choose(&ranges, d, &want(None, None)),
            (2, 48_000),
            "no wish = default"
        );
        assert_eq!(choose(&ranges, d, &want(Some(96_000), None)), (2, 96_000));
        assert_eq!(
            choose(&ranges, d, &want(Some(192_000), None)),
            (2, 96_000),
            "clamped"
        );
        assert_eq!(choose(&ranges, d, &want(Some(22_050), None)), (2, 44_100));
        assert_eq!(choose(&ranges, d, &want(None, Some(8))), (8, 48_000));
        assert_eq!(
            choose(&ranges, d, &want(Some(96_000), Some(8))),
            (8, 48_000)
        );
        assert_eq!(
            choose(&ranges, d, &want(Some(96_000), Some(5))),
            (2, 96_000),
            "no 5 ch: keep 2"
        );
        assert_eq!(
            choose(&[], d, &want(Some(96_000), Some(5))),
            d,
            "device lists nothing"
        );
        assert_eq!(buffer_choice(Some(64), Some((128, 4096))), Some(128));
        assert_eq!(buffer_choice(Some(9999), Some((128, 4096))), Some(4096));
        assert_eq!(buffer_choice(None, Some((128, 4096))), None);
        assert_eq!(
            buffer_choice(Some(256), None),
            None,
            "unknown range: host default"
        );
    }

    #[test]
    fn failed_reconfigure_falls_back_to_the_old_settings() {
        let old = OutConfig::default();
        let bad = OutConfig {
            sample_rate: Some(12_345),
            ..Default::default()
        };
        let slot = Mutex::new(old.clone());
        let good = Mutex::new(old.clone());
        let err = Mutex::new(None);
        let ok = |c: &OutConfig| {
            if c.sample_rate.is_some() {
                Err("no such rate".to_string())
            } else {
                Ok(c.clone())
            }
        };
        assert!(open_or_fall_back(&slot, &good, &err, ok).is_ok());
        assert!(err.lock().unwrap().is_none());
        *slot.lock().unwrap() = bad;
        let got = open_or_fall_back(&slot, &good, &err, ok).unwrap();
        assert_eq!(got, old, "reopened the previous settings");
        assert_eq!(*slot.lock().unwrap(), old);
        assert_eq!(err.lock().unwrap().as_deref(), Some("no such rate"));
    }

    #[test]
    fn changed_config_reopens_the_running_stream_with_the_new_wishes() {
        use std::sync::atomic::AtomicUsize;
        let slot = Arc::new(Mutex::new(OutConfig::default()));
        let failed = Arc::new(AtomicBool::new(false));
        let opened: Arc<Mutex<Vec<OutConfig>>> = Arc::default();
        let n = Arc::new(AtomicUsize::new(0));
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (s2, f2, o2, n2) = (slot.clone(), failed.clone(), opened.clone(), n.clone());
        let h = std::thread::spawn(move || {
            supervise(
                0u32,
                || {
                    o2.lock().unwrap().push(s2.lock().unwrap().clone());
                    n2.fetch_add(1, Ordering::SeqCst);
                    Ok((1u32, 48_000))
                },
                &AtomicU32::new(0),
                &f2,
                &stop_rx,
                Duration::from_millis(10),
                Duration::from_millis(5),
            )
        });
        let wait = |want: usize| {
            for _ in 0..300 {
                if n.load(Ordering::SeqCst) >= want {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let same = OutConfig::default();
        assert!(
            !swap_cfg(&slot, &failed, same),
            "nothing changed: no reopen"
        );
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(n.load(Ordering::SeqCst), 0);
        let wish = OutConfig {
            device: Some("USB".into()),
            sample_rate: Some(96_000),
            buffer_frames: Some(128),
            channels: Some(2),
        };
        assert!(swap_cfg(&slot, &failed, wish.clone()));
        wait(1);
        assert_eq!(
            opened.lock().unwrap().as_slice(),
            std::slice::from_ref(&wish)
        );
        assert!(
            !swap_cfg(&slot, &failed, wish),
            "same wishes again: stays open"
        );
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(n.load(Ordering::SeqCst), 1);
        drop(stop_tx);
        h.join().unwrap();
    }

    #[test]
    fn flush_without_callback_does_not_block_past_timeout() {
        let v = Voice::new(64);
        v.ring.push(&[0.9, 0.9]); // stale audio
        v.request_flush(); // no cpal callback will ever apply it
        assert_eq!(push_after_flush(&v, &[0.1, 0.1], 0, Duration::ZERO), None);
        let t = Instant::now();
        let n = loop {
            if let Some(n) = push_after_flush(&v, &[0.1, 0.1], 0, t.elapsed()) {
                break n;
            }
            std::thread::sleep(Duration::from_millis(2));
            assert!(t.elapsed() < Duration::from_secs(1), "blocked");
        };
        assert_eq!(n, 2);
        assert!(!v.flush_pending());
        assert_eq!(v.ring.pop_frame(), Some([0.1, 0.1])); // stale frame dropped
        assert_eq!(v.ring.pop_frame(), None);
    }

    #[test]
    fn decoder_follows_reopened_device_rate() {
        let Some(p) = make_media(
            "rate.mkv",
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=44100:duration=1",
                "-c:a",
                "pcm_s16le",
            ],
        ) else {
            eprintln!("ffmpeg CLI missing: skipped");
            return;
        };
        let rate = Arc::new(AtomicU32::new(44100));
        let mut d = AudioDecoder::open(&p, 44100).unwrap().expect("audio");
        assert!(!follow_rate(&mut d, &rate));
        // Device lost, reopened at 48 kHz by the supervisor.
        let failed = Arc::new(AtomicBool::new(true));
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let (r2, f2) = (rate.clone(), failed.clone());
        let h = std::thread::spawn(move || {
            supervise(
                0u32,
                || Ok((1u32, 48000)),
                &r2,
                &f2,
                &stop_rx,
                Duration::from_millis(5),
                Duration::from_millis(5),
            )
        });
        for _ in 0..200 {
            if rate.load(Ordering::Acquire) == 48000 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(stop_tx);
        h.join().unwrap();
        assert!(follow_rate(&mut d, &rate));
        d.seek(0.0).unwrap();
        let mut out = vec![];
        while d.read_chunk(&mut out).unwrap() > 0 {}
        let frames = out.len() / 2;
        assert!(
            (47000..=49000).contains(&frames),
            "expected ~48000 frames per second of media, got {frames}"
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn resync_window() {
        assert!(!needs_resync(1.1, 1.0));
        assert!(!needs_resync(1.0, 1.0));
        assert!(needs_resync(0.5, 1.0)); // far behind
        assert!(needs_resync(3.0, 1.0)); // far ahead (loop wrapped back)
    }

    /// Generates a media file with the ffmpeg CLI; None (test skipped) when it is missing.
    fn make_media(name: &str, args: &[&str]) -> Option<std::path::PathBuf> {
        let dir = std::env::temp_dir().join(format!("vjc-audio-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let p = dir.join(name);
        let ok = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .arg(&p)
            .status()
            .ok()?
            .success();
        ok.then_some(p)
    }

    #[test]
    fn decodes_and_resamples_generated_file() {
        // 1 s mono 440 Hz sine at 44.1 kHz inside a video container with a video track.
        let Some(p) = make_media(
            "sine.mkv",
            &[
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=44100:duration=1",
                "-f",
                "lavfi",
                "-i",
                "color=c=red:s=64x64:d=1:r=10",
                "-c:v",
                "mjpeg",
                "-c:a",
                "pcm_s16le",
            ],
        ) else {
            eprintln!("ffmpeg CLI missing: skipped");
            return;
        };
        let mut d = AudioDecoder::open(&p, 48000)
            .unwrap()
            .expect("audio stream");
        let mut out = vec![];
        while d.read_chunk(&mut out).unwrap() > 0 {}
        let frames = out.len() / 2;
        assert!(
            (47000..=49000).contains(&frames),
            "expected ~48000 frames, got {frames}"
        );
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05 && peak <= 1.0, "peak {peak}");
        // mono -> stereo: both channels carry the same signal
        assert!(
            out.as_chunks::<2>()
                .0
                .iter()
                .all(|f| (f[0] - f[1]).abs() < 1e-6)
        );
        assert!(d.at_end());
        // seek back and read again
        d.seek(0.5).unwrap();
        let mut again = vec![];
        assert!(d.read_chunk(&mut again).unwrap() > 0);
        assert!(d.pos() >= 0.4, "pos {}", d.pos());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn file_without_audio_has_no_decoder() {
        let Some(p) = make_media(
            "silent.mkv",
            &[
                "-f",
                "lavfi",
                "-i",
                "color=c=blue:s=64x64:d=1:r=10",
                "-c:v",
                "mjpeg",
            ],
        ) else {
            eprintln!("ffmpeg CLI missing: skipped");
            return;
        };
        assert!(AudioDecoder::open(&p, 48000).unwrap().is_none());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    #[ignore = "needs a real audio output device"]
    fn engine_opens_default_device() {
        let e = AudioEngine::start().unwrap();
        assert!(e.sample_rate() > 0);
    }
}

#[cfg(test)]
mod device_tests {
    #[test]
    fn listing_and_unknown_device_do_not_panic() {
        // No device on CI: the list is empty, and an unknown name falls back to the default.
        let names = super::output_devices();
        assert!(names.iter().all(|n| !n.is_empty()) || names.is_empty());
        let _ = super::pick_output(Some("no such device \u{1}"));
    }

    #[test]
    fn busy_voice_list_still_feeds_the_tap_with_silence() {
        let voices: super::VoiceList = Default::default();
        let tap = super::Tap::new();
        tap.start();
        let (mut st, mut tp) = (vec![1.0f32; 64], vec![0f32; 64]);
        let _held = voices.lock().unwrap();
        let mut fx = super::MixFx::new(
            std::sync::Arc::new(crate::audio_fx::Buses::new()),
            48_000,
            64,
        );
        super::render_block(&voices, &tap, &mut fx, &mut st, &mut tp);
        assert!(st.iter().all(|s| *s == 0.0));
        assert_eq!(tap.len_frames(), 32, "tap counts the silent block");
        assert_eq!(tap.pop_frame(), Some([0.0, 0.0]));
    }
}
