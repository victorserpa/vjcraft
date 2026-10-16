//! Live audio analysis: mono input samples in, band levels out. The input callback only
//! buffers; every `FFT_SIZE` samples it runs one small FFT and publishes five levels
//! (low, low-mid, mid, high, overall) as atomics, so readers on the UI thread never block.
//! The same block also yields a log-spaced spectrum and a downsampled waveform (for ISF
//! `audioFFT` / `audio` inputs), published through a lock the audio thread only `try_lock`s.

use rustfft::{FftPlanner, num_complex::Complex};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub const FFT_SIZE: usize = 1024;
/// Published levels: low, low-mid, mid, high, overall.
pub const BANDS: usize = 5;
/// Log-spaced spectrum bins published per block.
pub const SPECTRUM_BINS: usize = 64;
/// Waveform points published per block.
pub const WAVE_POINTS: usize = 128;
/// Lowest and highest spectrum frequency, in Hz.
const SPEC_LO: f32 = 40.0;
const SPEC_HI: f32 = 16_000.0;
/// Band edges in Hz: low < 250 <= low-mid < 1000 <= mid < 4000 <= high.
const EDGES: [f32; 3] = [250.0, 1000.0, 4000.0];

/// Levels 0..=1 of `samples` (a Hann window is applied; length should be a power of two).
/// Band levels are the amplitude of the strongest tone-equivalent in the band; the last
/// entry is the RMS of the whole signal.
pub fn band_levels(samples: &[f32], rate: u32) -> [f32; BANDS] {
    let n = samples.len();
    if n < 8 || rate == 0 {
        return [0.0; BANDS];
    }
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    band_levels_with(&*fft, samples, rate)
}

fn band_levels_with(fft: &dyn rustfft::Fft<f32>, samples: &[f32], rate: u32) -> [f32; BANDS] {
    bands_of(&windowed_fft(fft, samples), samples, rate)
}

fn windowed_fft(fft: &dyn rustfft::Fft<f32>, samples: &[f32]) -> Vec<Complex<f32>> {
    let n = samples.len();
    let mut buf: Vec<Complex<f32>> = samples
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos();
            Complex::new(if s.is_finite() { s * w } else { 0.0 }, 0.0)
        })
        .collect();
    fft.process(&mut buf);
    buf
}

fn bands_of(buf: &[Complex<f32>], samples: &[f32], rate: u32) -> [f32; BANDS] {
    let n = samples.len();
    // A sine of amplitude A peaks at A*n/4 with a Hann window, spreading over ~3 bins.
    let norm = n as f32 / 4.0;
    let hz = rate as f32 / n as f32;
    let mut energy = [0.0f32; 4];
    for (k, c) in buf.iter().enumerate().take(n / 2).skip(1) {
        let f = k as f32 * hz;
        let b = EDGES.iter().take_while(|&&e| f >= e).count();
        energy[b] += c.norm_sqr();
    }
    let mut out = [0.0f32; BANDS];
    for (o, e) in out.iter_mut().zip(energy) {
        // main lobe of a Hann-windowed tone holds ~1.5x the peak bin's energy
        *o = (e.sqrt() / (norm * 1.2247)).clamp(0.0, 1.0);
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / n as f32).sqrt();
    out[4] = (rms * std::f32::consts::SQRT_2).clamp(0.0, 1.0);
    out
}

/// Spectrum of a windowed FFT as `SPECTRUM_BINS` log-spaced levels 0..=1 (a sine of
/// amplitude A reads about A in its bin, lightly compressed so quiet tones show).
fn spectrum_of(buf: &[Complex<f32>], rate: u32) -> [f32; SPECTRUM_BINS] {
    let n = buf.len();
    let mut out = [0.0f32; SPECTRUM_BINS];
    if n < 8 || rate == 0 {
        return out;
    }
    let hz = rate as f32 / n as f32;
    let hi = SPEC_HI.min(rate as f32 / 2.0);
    let ratio = (hi / SPEC_LO).powf(1.0 / SPECTRUM_BINS as f32);
    let norm = n as f32 / 4.0;
    for (i, o) in out.iter_mut().enumerate() {
        let (f0, f1) = (
            SPEC_LO * ratio.powi(i as i32),
            SPEC_LO * ratio.powi(i as i32 + 1),
        );
        // At least the FFT bin under the band's centre.
        let k0 = ((f0 / hz).floor() as usize).clamp(1, n / 2 - 1);
        let k1 = ((f1 / hz).ceil() as usize).clamp(k0 + 1, n / 2);
        let peak = buf[k0..k1].iter().map(|c| c.norm()).fold(0.0f32, f32::max);
        *o = (peak / norm).clamp(0.0, 1.0).sqrt();
    }
    out
}

/// Waveform of `samples` as `WAVE_POINTS` points in 0..=1 (0.5 = silence).
fn wave_of(samples: &[f32]) -> [f32; WAVE_POINTS] {
    let mut out = [0.5f32; WAVE_POINTS];
    if samples.is_empty() {
        return out;
    }
    for (i, o) in out.iter_mut().enumerate() {
        let a = i * samples.len() / WAVE_POINTS;
        let b = ((i + 1) * samples.len() / WAVE_POINTS).clamp(a + 1, samples.len());
        let mean = samples[a..b].iter().filter(|s| s.is_finite()).sum::<f32>() / (b - a) as f32;
        *o = (0.5 + 0.5 * mean).clamp(0.0, 1.0);
    }
    out
}

/// Spectrum and waveform of one analysed block.
#[derive(Debug, Clone, PartialEq)]
pub struct Detail {
    pub spectrum: [f32; SPECTRUM_BINS],
    pub wave: [f32; WAVE_POINTS],
}

impl Default for Detail {
    fn default() -> Self {
        Detail {
            spectrum: [0.0; SPECTRUM_BINS],
            wave: [0.5; WAVE_POINTS],
        }
    }
}

/// Spectrum and waveform of `samples` (same windowing as `band_levels`).
pub fn detail(samples: &[f32], rate: u32) -> Detail {
    let n = samples.len();
    if n < 8 || rate == 0 {
        return Detail::default();
    }
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    detail_with(&*fft, samples, rate).1
}

fn detail_with(fft: &dyn rustfft::Fft<f32>, samples: &[f32], rate: u32) -> ([f32; BANDS], Detail) {
    let buf = windowed_fft(fft, samples);
    (
        bands_of(&buf, samples, rate),
        Detail {
            spectrum: spectrum_of(&buf, rate),
            wave: wave_of(samples),
        },
    )
}

/// Levels shared between the analysis (audio thread) and readers: f32 bits in atomics.
#[derive(Debug, Default)]
pub struct SharedBands([AtomicU32; BANDS], AtomicU32, std::sync::Mutex<Detail>);

impl SharedBands {
    pub fn store(&self, v: &[f32; BANDS]) {
        for (a, x) in self.0.iter().zip(v) {
            a.store(x.to_bits(), Ordering::Relaxed);
        }
        self.1.fetch_add(1, Ordering::Release);
    }
    /// Publishes the spectrum and waveform; skipped (the reader keeps the last ones) if a
    /// reader holds the lock at that instant, so the audio thread never waits.
    pub fn store_detail(&self, d: &Detail) {
        if let Ok(mut g) = self.2.try_lock() {
            g.clone_from(d);
        }
    }
    pub fn load_detail(&self) -> Detail {
        self.2.lock().map(|g| g.clone()).unwrap_or_default()
    }
    /// Number of `store` calls so far; a reader tells a stalled input by it not moving.
    pub fn blocks(&self) -> u32 {
        self.1.load(Ordering::Acquire)
    }
    pub fn load(&self) -> [f32; BANDS] {
        let mut o = [0.0; BANDS];
        for (x, a) in o.iter_mut().zip(&self.0) {
            *x = f32::from_bits(a.load(Ordering::Relaxed));
        }
        o
    }
}

/// Builds the input sink: buffers samples and publishes levels each `FFT_SIZE` samples.
/// Hand it to `LtcInput::start` (which opens the input stream and passes the rate).
pub fn sink(shared: Arc<SharedBands>, rate: u32) -> impl FnMut(&[f32]) + Send + 'static {
    sink_sized(shared, rate, FFT_SIZE)
}

/// Like `sink` with a chosen block size (a power of two from 64 to 16384; anything else
/// becomes `FFT_SIZE`). Bigger blocks resolve bass better, smaller ones answer faster.
pub fn sink_sized(
    shared: Arc<SharedBands>,
    rate: u32,
    size: usize,
) -> impl FnMut(&[f32]) + Send + 'static {
    let size = if size.is_power_of_two() && (64..=16_384).contains(&size) {
        size
    } else {
        FFT_SIZE
    };
    let fft = FftPlanner::<f32>::new().plan_fft_forward(size);
    let mut buf: Vec<f32> = Vec::with_capacity(size);
    move |data: &[f32]| {
        for &s in data {
            buf.push(s);
            if buf.len() == size {
                let (bands, d) = detail_with(&*fft, &buf, rate);
                shared.store_detail(&d);
                shared.store(&bands);
                buf.clear();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, rate: u32, amp: f32) -> Vec<f32> {
        (0..FFT_SIZE)
            .map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn spectrum_peaks_at_the_tone_and_wave_follows_the_signal() {
        let d = detail(&sine(1000.0, 48_000, 0.8), 48_000);
        let top = d
            .spectrum
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        // 1 kHz on a log axis from 40 Hz to 16 kHz.
        let want =
            ((1000.0f32 / SPEC_LO).ln() / (SPEC_HI / SPEC_LO).ln() * SPECTRUM_BINS as f32) as usize;
        assert!(top.abs_diff(want) <= 2, "peak {top}, want ~{want}");
        assert!(d.spectrum[top] > 0.5);
        assert!(d.spectrum[2] < 0.1 && d.spectrum[SPECTRUM_BINS - 2] < 0.1);
        assert!(d.wave.iter().all(|w| (0.0..=1.0).contains(w)));
        assert!(d.wave.iter().any(|w| (w - 0.5).abs() > 0.1));
        assert_eq!(detail(&[], 48_000), Detail::default());
    }

    #[test]
    fn sized_sink_publishes_once_per_chosen_block() {
        let shared = Arc::new(SharedBands::default());
        let mut s = sink_sized(shared.clone(), 48_000, 2048);
        let x: Vec<f32> = (0..4096)
            .map(|i| 0.8 * (std::f32::consts::TAU * 100.0 * i as f32 / 48_000.0).sin())
            .collect();
        s(&x[..2047]);
        assert_eq!(shared.blocks(), 0);
        s(&x[2047..]);
        assert_eq!(shared.blocks(), 2, "4096 samples = two 2048 blocks");
        assert!(shared.load()[0] > 0.5);
        let mut bad = sink_sized(shared.clone(), 48_000, 1000);
        bad(&x[..FFT_SIZE]);
        assert_eq!(shared.blocks(), 3, "invalid size falls back to FFT_SIZE");
    }

    #[test]
    fn low_sine_lights_only_low_band() {
        let l = band_levels(&sine(100.0, 48_000, 0.8), 48_000);
        assert!(l[0] > 0.6, "low {l:?}");
        assert!(l[1] < 0.05 && l[2] < 0.05 && l[3] < 0.05, "others {l:?}");
        assert!(l[4] > 0.5);
    }

    #[test]
    fn each_band_follows_its_tone() {
        for (hz, idx) in [(500.0, 1), (2000.0, 2), (8000.0, 3)] {
            let l = band_levels(&sine(hz, 48_000, 0.8), 48_000);
            assert!(l[idx] > 0.6, "{hz} Hz -> {l:?}");
            for (i, v) in l.iter().enumerate().take(4) {
                if i != idx {
                    assert!(*v < 0.05, "{hz} Hz leaks into {i}: {l:?}");
                }
            }
        }
    }

    #[test]
    fn silence_and_garbage_are_zero() {
        assert_eq!(band_levels(&vec![0.0; FFT_SIZE], 48_000), [0.0; BANDS]);
        assert_eq!(band_levels(&[], 48_000), [0.0; BANDS]);
        let nan = vec![f32::NAN; FFT_SIZE];
        assert_eq!(band_levels(&nan, 48_000)[..4], [0.0; 4]);
    }

    #[test]
    fn sink_publishes_after_a_full_block() {
        let sh = Arc::new(SharedBands::default());
        let mut s = sink(sh.clone(), 48_000);
        let x = sine(100.0, 48_000, 0.8);
        s(&x[..FFT_SIZE - 1]);
        assert_eq!(sh.load(), [0.0; BANDS]);
        s(&x[FFT_SIZE - 1..]);
        assert!(sh.load()[0] > 0.6);
    }
}
