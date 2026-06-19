//! Fixed log-linear latency histogram. Bounded memory, cheap to record into and merge, and
//! percentiles are reported as bucket upper bounds (honest over-estimates).

/// Bucket upper bounds in milliseconds. A recorded value lands in the first bucket whose bound
/// is `>= value`; anything larger lands in an implicit overflow bucket.
const BOUNDS_MS: &[u64] = &[
    1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
];

#[derive(Clone, Debug)]
pub struct Histogram {
    buckets: Vec<u64>, // len = BOUNDS_MS.len() + 1 (last is the overflow bucket)
    count: u64,
    max: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: vec![0; BOUNDS_MS.len() + 1],
            count: 0,
            max: 0,
        }
    }
}

impl Histogram {
    pub fn record(&mut self, ms: u64) {
        let idx = BOUNDS_MS
            .iter()
            .position(|&b| ms <= b)
            .unwrap_or(BOUNDS_MS.len());
        self.buckets[idx] += 1;
        self.count += 1;
        if ms > self.max {
            self.max = ms;
        }
    }

    pub fn merge(&mut self, other: &Histogram) {
        for (i, c) in other.buckets.iter().enumerate() {
            self.buckets[i] += c;
        }
        self.count += other.count;
        if other.max > self.max {
            self.max = other.max;
        }
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn max(&self) -> u64 {
        self.max
    }

    /// Percentile in milliseconds, reported as the upper bound of the bucket the rank falls in.
    /// The overflow bucket reports the exact observed `max`. Returns 0 for an empty histogram.
    pub fn percentile(&self, q: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = ((q * self.count as f64).ceil() as u64).max(1);
        let mut cum = 0u64;
        for (i, c) in self.buckets.iter().enumerate() {
            cum += c;
            if cum >= target {
                return BOUNDS_MS.get(i).copied().unwrap_or(self.max);
            }
        }
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_reports_zero() {
        let h = Histogram::default();
        assert_eq!(h.count(), 0);
        assert_eq!(h.percentile(0.5), 0);
        assert_eq!(h.max(), 0);
    }

    #[test]
    fn single_value_uses_bucket_bound_and_exact_max() {
        let mut h = Histogram::default();
        h.record(100); // first bound >= 100 is 128
        assert_eq!(h.count(), 1);
        assert_eq!(h.percentile(0.5), 128);
        assert_eq!(h.percentile(0.99), 128);
        assert_eq!(h.max(), 100);
    }

    #[test]
    fn skewed_distribution_percentiles() {
        let mut h = Histogram::default();
        for _ in 0..90 {
            h.record(1);
        }
        for _ in 0..10 {
            h.record(1000); // first bound >= 1000 is 1024
        }
        assert_eq!(h.count(), 100);
        assert_eq!(h.percentile(0.50), 1);
        assert_eq!(h.percentile(0.95), 1024);
        assert_eq!(h.percentile(0.99), 1024);
        assert_eq!(h.max(), 1000);
    }

    #[test]
    fn overflow_bucket_reports_max() {
        let mut h = Histogram::default();
        h.record(200_000); // larger than the last bound (65536) -> overflow bucket
        assert_eq!(h.percentile(0.99), 200_000);
        assert_eq!(h.max(), 200_000);
    }

    #[test]
    fn merge_combines_counts_and_max() {
        let mut a = Histogram::default();
        a.record(5);
        let mut b = Histogram::default();
        b.record(5000);
        a.merge(&b);
        assert_eq!(a.count(), 2);
        assert_eq!(a.max(), 5000);
    }
}
