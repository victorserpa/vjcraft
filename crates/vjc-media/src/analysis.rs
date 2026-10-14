//! Live audio analysis: mono input samples in, band levels out. The input callback only
//! buffers; every `FFT_SIZE` samples it runs one small FFT and publishes five levels
//! (low, low-mid, mid, high, overall) as atomics, so readers on the UI thread never block.

use rustfft::{FftPlanner, num_complex::Complex};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

pub const FFT_SIZE: usize = 1024;
/// Published levels: low, low-mid, mid, high, overall.
pub const BANDS: usize = 5;
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

/// Levels shared between the analysis (audio thread) and readers: f32 bits in atomics.
#[derive(Debug, Default)]
pub struct SharedBands([AtomicU32; BANDS], AtomicU32);

impl SharedBands {
    pub fn store(&self, v: &[f32; BANDS]) {
        for (a, x) in self.0.iter().zip(v) {
            a.store(x.to_bits(), Ordering::Relaxed);
        }
        self.1.fetch_add(1, Ordering::Release);
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
    let fft = FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
    let mut buf: Vec<f32> = Vec::with_capacity(FFT_SIZE);
    move |data: &[f32]| {
        for &s in data {
            buf.push(s);
            if buf.len() == FFT_SIZE {
                shared.store(&band_levels_with(&*fft, &buf, rate));
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
