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
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ffmpeg_next as ff;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Ring capacity per voice, in seconds of audio.
const RING_SECONDS: f64 = 0.5;
/// Decoder keeps the ring filled up to this much audio (the audio latency vs the video).
const TARGET_LEAD: f64 = 0.15;
/// Frames mixed per pass in the cpal callback (scratch is preallocated for this).
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

    /// Consumer: drops everything buffered.
    pub fn skip_all(&self) {
        self.read
            .store(self.write.load(Ordering::Acquire), Ordering::Release);
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
    flush: AtomicBool,
}

impl Voice {
    pub fn new(ring_frames: usize) -> Voice {
        Voice {
            ring: Ring::new(ring_frames),
            volume: AtomicU32::new(1.0f32.to_bits()),
            pan: AtomicU32::new(0.0f32.to_bits()),
            audible: AtomicBool::new(false),
            flush: AtomicBool::new(false),
        }
    }

    /// Linear volume (>= 0), pan -1 (left) .. 1 (right), and whether to play at all.
    pub fn set(&self, volume: f32, pan: f32, audible: bool) {
        self.volume.store(volume.to_bits(), Ordering::Relaxed);
        self.pan.store(pan.to_bits(), Ordering::Relaxed);
        self.audible.store(audible, Ordering::Relaxed);
    }

    fn audible(&self) -> bool {
        self.audible.load(Ordering::Relaxed)
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

/// Mixes every voice into `out` (interleaved stereo, overwritten) and clamps to [-1, 1].
/// Allocation-free; safe for the audio callback.
pub fn mix_stereo(out: &mut [f32], voices: &[Arc<Voice>]) {
    out.fill(0.0);
    for v in voices {
        if !v.audible() {
            v.ring.skip_all();
            continue;
        }
        if v.flush.swap(false, Ordering::Acquire) {
            v.ring.skip_all();
        }
        let vol = f32::from_bits(v.volume.load(Ordering::Relaxed));
        let vol = if vol.is_finite() { vol.max(0.0) } else { 0.0 };
        let (gl, gr) = pan_gains(f32::from_bits(v.pan.load(Ordering::Relaxed)));
        for frame in out.as_chunks_mut::<2>().0 {
            let Some([l, r]) = v.ring.pop_frame() else {
                break;
            };
            frame[0] += l * vol * gl;
            frame[1] += r * vol * gr;
        }
    }
    for s in out.iter_mut() {
        *s = s.clamp(-1.0, 1.0);
    }
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
    rate: u32,
    _guard: Arc<Guard>,
}

impl AudioEngine {
    /// Opens the default output device. Needs a real audio device (not covered by CI tests).
    pub fn start() -> Result<AudioEngine, String> {
        let voices: VoiceList = Arc::new(Mutex::new(Vec::new()));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
        let (stop_tx, stop_rx) = mpsc::channel::<()>();
        let v = voices.clone();
        let thread = std::thread::Builder::new()
            .name("vjc-audio-out".into())
            .spawn(move || {
                // The cpal stream is not Send: it lives and dies on this thread.
                match open_stream(v) {
                    Ok((stream, rate)) => {
                        let _ = ready_tx.send(Ok(rate));
                        let _ = stop_rx.recv(); // returns when the engine is dropped
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let rate = ready_rx
            .recv()
            .map_err(|_| "audio thread ended unexpectedly".to_string())??;
        Ok(AudioEngine {
            voices,
            rate,
            _guard: Arc::new(Guard {
                stop: Some(stop_tx),
                thread: Some(thread),
            }),
        })
    }

    /// Device sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn add_voice(&self) -> Arc<Voice> {
        let v = Arc::new(Voice::new((self.rate as f64 * RING_SECONDS) as usize));
        self.voices.lock().unwrap().push(v.clone());
        v
    }

    fn remove_voice(&self, v: &Arc<Voice>) {
        self.voices.lock().unwrap().retain(|x| !Arc::ptr_eq(x, v));
    }
}

fn open_stream(voices: VoiceList) -> Result<(cpal::Stream, u32), String> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or("No audio output device")?;
    let supported = device
        .default_output_config()
        .map_err(|e| format!("Audio output config: {e}"))?;
    let cfg = supported.config();
    let rate = cfg.sample_rate;
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => build::<f32>(&device, cfg, voices),
        cpal::SampleFormat::I16 => build::<i16>(&device, cfg, voices),
        cpal::SampleFormat::U16 => build::<u16>(&device, cfg, voices),
        cpal::SampleFormat::I32 => build::<i32>(&device, cfg, voices),
        other => return Err(format!("Unsupported audio sample format {other}")),
    }
    .map_err(|e| format!("Audio output: {e}"))?;
    stream.play().map_err(|e| format!("Audio output: {e}"))?;
    Ok((stream, rate))
}

fn build<T>(
    device: &cpal::Device,
    cfg: cpal::StreamConfig,
    voices: VoiceList,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let ch = (cfg.channels as usize).max(1);
    let mut scratch = vec![0f32; SCRATCH_FRAMES * 2];
    device.build_output_stream(
        cfg,
        move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
            for chunk in data.chunks_mut(SCRATCH_FRAMES * ch) {
                let frames = chunk.len() / ch;
                let st = &mut scratch[..frames * 2];
                match voices.try_lock() {
                    Ok(g) => mix_stereo(st, &g),
                    Err(_) => st.fill(0.0),
                }
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
        |e| eprintln!("vjc-media: audio stream error: {e}"),
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
            engine.sample_rate(),
        );
        let thread = std::thread::Builder::new()
            .name("vjc-audio-dec".into())
            .spawn(move || run_audio(&p, rate, &v, &shared, &s))
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

fn run_audio(path: &Path, rate: u32, voice: &Voice, shared: &Shared, stop: &AtomicBool) {
    let mut dec = match AudioDecoder::open(path, rate) {
        Ok(Some(d)) => d,
        Ok(None) => return,
        Err(e) => {
            eprintln!("vjc-media: audio of {} unavailable: {e}", path.display());
            return;
        }
    };
    let nap = |ms| std::thread::sleep(Duration::from_millis(ms));
    let target = (rate as f64 * TARGET_LEAD) as usize;
    let mut buf: Vec<f32> = Vec::new();
    let mut off = 0usize;
    let mut synced = false;
    while !stop.load(Ordering::Relaxed) && !shared.stop.load(Ordering::Relaxed) {
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
            buf.clear();
            off = 0;
            synced = true;
        }
        if off < buf.len() {
            off += voice.ring.push(&buf[off..]);
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
