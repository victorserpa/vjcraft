//! Rolling frame-time counter (status bar and `VJC_FRAME_LOG`).

use std::collections::VecDeque;

const WINDOW: usize = 240;

pub struct FrameStats {
    ms: VecDeque<f32>,
}

impl FrameStats {
    pub fn new() -> Self {
        FrameStats {
            ms: VecDeque::with_capacity(WINDOW),
        }
    }

    pub fn push(&mut self, ms: f32) {
        if self.ms.len() == WINDOW {
            self.ms.pop_front();
        }
        self.ms.push_back(ms);
    }

    pub fn len(&self) -> usize {
        self.ms.len()
    }

    pub fn avg(&self) -> f32 {
        if self.ms.is_empty() {
            return 0.0;
        }
        self.ms.iter().sum::<f32>() / self.ms.len() as f32
    }

    /// 99th percentile of the window (nearest rank).
    pub fn p99(&self) -> f32 {
        if self.ms.is_empty() {
            return 0.0;
        }
        let mut v: Vec<f32> = self.ms.iter().copied().collect();
        v.sort_by(f32::total_cmp);
        v[((v.len() * 99).div_ceil(100)).max(1) - 1]
    }

    /// Status bar text, e.g. `16.7 ms (p99 21.0)`.
    pub fn label(&self) -> String {
        format!("{:.1} ms (p99 {:.1})", self.avg(), self.p99())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p99_sees_the_spike_avg_does_not() {
        let mut s = FrameStats::new();
        for _ in 0..99 {
            s.push(10.0);
        }
        s.push(100.0);
        assert_eq!(s.p99(), 10.0);
        // 100 samples: nearest rank 99 is still 10; add one more spike to cross it.
        s.push(100.0);
        assert_eq!(s.p99(), 100.0);
        assert!(s.avg() < 15.0, "avg hides the spikes");
    }

    #[test]
    fn negative_control_flat_series_has_no_spike() {
        let mut s = FrameStats::new();
        for _ in 0..50 {
            s.push(10.0);
        }
        assert_eq!(s.p99(), 10.0);
        assert_eq!(s.avg(), 10.0);
    }

    #[test]
    fn window_is_bounded() {
        let mut s = FrameStats::new();
        for i in 0..1000 {
            s.push(i as f32);
        }
        assert_eq!(s.len(), WINDOW);
    }
}
