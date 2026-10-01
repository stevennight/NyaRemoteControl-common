//! Percentiles for the statistics both ends report (ServerStats, the client's
//! statistics panel).

use std::collections::VecDeque;

/// `p` in 0..=1 of `v` (sorted in place); 0 for no samples.
pub fn percentile(v: &mut [f32], p: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    v[((v.len() - 1) as f32 * p).round() as usize]
}

/// Samples of the last few reporting intervals. A one-second interval holds
/// only a few dozen frames, too few for a meaningful 99th percentile.
pub struct Rolling {
    intervals: VecDeque<Vec<f32>>,
    keep: usize,
}

impl Rolling {
    pub fn new(keep: usize) -> Self {
        Self { intervals: VecDeque::new(), keep: keep.max(1) }
    }

    /// Close an interval: (median of this interval, 99th percentile over the
    /// kept intervals).
    pub fn close(&mut self, interval: Vec<f32>) -> (f32, f32) {
        let mut cur = interval.clone();
        let p50 = percentile(&mut cur, 0.5);
        self.intervals.push_back(interval);
        while self.intervals.len() > self.keep {
            self.intervals.pop_front();
        }
        let mut all: Vec<f32> = self.intervals.iter().flatten().copied().collect();
        (p50, percentile(&mut all, 0.99))
    }

    pub fn clear(&mut self) {
        self.intervals.clear();
    }
}

impl Default for Rolling {
    /// Ten one-second intervals.
    fn default() -> Self {
        Self::new(10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        assert_eq!(percentile(&mut [], 0.5), 0.0);
        let mut v: Vec<f32> = (1..=100).rev().map(|x| x as f32).collect();
        assert_eq!(percentile(&mut v, 0.5), 51.0);
        assert_eq!(percentile(&mut v, 0.99), 99.0);
        assert_eq!(percentile(&mut v, 1.0), 100.0);
    }

    #[test]
    fn rolling_window_keeps_the_last_intervals() {
        let mut r = Rolling::new(2);
        assert_eq!(r.close((1..=100).map(|x| x as f32).collect()), (51.0, 99.0));
        // 200 samples: the 99th percentile is the third largest.
        assert_eq!(r.close(vec![0.0; 100]), (0.0, 98.0));
        // The first interval left the window.
        assert_eq!(r.close(vec![0.0; 100]), (0.0, 0.0));
    }
}
