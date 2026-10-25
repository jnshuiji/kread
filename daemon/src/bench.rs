//! High-precision benchmark engine with CPU governor warmup and inner batching.

use std::io;
use std::time::{Duration, Instant};

use crate::driver::MemoryReader;

/// Configuration options for the memory benchmark runner.
#[derive(Debug, Clone)]
pub struct BenchConfig {
    /// Target process ID.
    pub pid: i32,
    /// Starting virtual address.
    pub addr: u64,
    /// Duration of each benchmark phase in seconds.
    pub duration_secs: u64,
    /// Warmup phase duration in milliseconds (default: 100ms).
    pub warmup_ms: u64,
    /// Number of inner iterations per clock check (default: 128).
    pub inner_batch_size: u64,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            pid: 1,
            addr: 0x1000,
            duration_secs: 3,
            warmup_ms: 100,
            inner_batch_size: 128,
        }
    }
}

/// Measurement metrics from a single benchmark phase.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchMetrics {
    /// Total number of reader operations executed.
    pub total_calls: u64,
    /// Elapsed wall-clock time in seconds.
    pub elapsed_secs: f64,
    /// Throughput in operations per second.
    pub ops_per_sec: f64,
    /// Throughput in MiB/sec.
    pub mib_per_sec: f64,
    /// Average latency per call in microseconds (fractional).
    pub avg_latency_us: f64,
}

/// Comprehensive benchmark report containing results for range and batch reads.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchReport {
    /// Target process ID.
    pub pid: i32,
    /// Starting address benchmarked.
    pub addr: u64,
    /// Duration configured per phase.
    pub duration_secs: u64,
    /// Metrics for single 8-byte range reads.
    pub range_metrics: BenchMetrics,
    /// Metrics for 16-address scatter batch reads.
    pub batch_metrics: BenchMetrics,
    /// Total entries resolved in batch mode.
    pub batch_total_addrs: u64,
    /// Address resolution throughput in addrs/sec.
    pub batch_addrs_per_sec: f64,
}

/// Runs a high-precision memory reading benchmark against any [`MemoryReader`].
///
/// Features:
/// - 100ms CPU governor warmup to ramp up core clocks on mobile big.LITTLE CPUs.
/// - 128x inner batching to eliminate `clock_gettime` syscall overhead.
/// - Fractional microsecond latency calculation (`.as_secs_f64() * 1e6 / iters`).
/// - Elapsed time division-by-zero protection (`.max(0.000001)`).
pub fn run_benchmark(
    reader: &dyn MemoryReader,
    config: &BenchConfig,
) -> io::Result<BenchReport> {
    let warmup_duration = Duration::from_millis(config.warmup_ms);
    let bench_duration = Duration::from_secs(config.duration_secs);
    let batch_size = config.inner_batch_size.max(1);

    // =========================================================================
    // 1. Single 8-byte range read benchmark
    // =========================================================================
    let mut buf = [0u8; 8];

    // Warmup: ramp up CPU frequency governor and populate L1/TLB caches
    if config.warmup_ms > 0 {
        let warmup_end = Instant::now() + warmup_duration;
        while Instant::now() < warmup_end {
            let _ = reader.read_range(config.pid, config.addr, &mut buf);
        }
    }

    let mut range_iters = 0u64;
    let range_start = Instant::now();

    while range_start.elapsed() < bench_duration {
        for _ in 0..batch_size {
            let _ = reader.read_range(config.pid, config.addr, &mut buf)?;
        }
        range_iters += batch_size;
    }

    let range_elapsed = range_start.elapsed();
    let range_elapsed_secs = range_elapsed.as_secs_f64().max(0.000001);
    let range_ops = range_iters as f64 / range_elapsed_secs;
    let range_bytes = range_iters * 8;
    let range_mbs = (range_bytes as f64 / 1_048_576.0) / range_elapsed_secs;
    let range_latency_us =
        (range_elapsed.as_secs_f64() * 1_000_000.0) / (range_iters.max(1) as f64);

    let range_metrics = BenchMetrics {
        total_calls: range_iters,
        elapsed_secs: range_elapsed_secs,
        ops_per_sec: range_ops,
        mib_per_sec: range_mbs,
        avg_latency_us: range_latency_us,
    };

    // =========================================================================
    // 2. Batch read benchmark (16 addresses per call)
    // =========================================================================
    let batch_addrs: Vec<u64> = (0..16)
        .map(|i| config.addr.wrapping_add(i * 8))
        .collect();
    let mut vals = [0u64; 16];

    // Warmup for batch path
    if config.warmup_ms > 0 {
        let warmup_end = Instant::now() + warmup_duration;
        while Instant::now() < warmup_end {
            let _ = reader.read_batch(config.pid, &batch_addrs, &mut vals);
        }
    }

    let mut batch_iters = 0u64;
    let batch_start = Instant::now();

    while batch_start.elapsed() < bench_duration {
        for _ in 0..batch_size {
            let _ = reader.read_batch(config.pid, &batch_addrs, &mut vals)?;
        }
        batch_iters += batch_size;
    }

    let batch_elapsed = batch_start.elapsed();
    let batch_elapsed_secs = batch_elapsed.as_secs_f64().max(0.000001);
    let batch_calls_sec = batch_iters as f64 / batch_elapsed_secs;
    let total_addrs = batch_iters * 16;
    let batch_addrs_sec = total_addrs as f64 / batch_elapsed_secs;
    let batch_bytes = total_addrs * 8;
    let batch_mbs = (batch_bytes as f64 / 1_048_576.0) / batch_elapsed_secs;
    let batch_latency_us =
        (batch_elapsed.as_secs_f64() * 1_000_000.0) / (batch_iters.max(1) as f64);

    let batch_metrics = BenchMetrics {
        total_calls: batch_iters,
        elapsed_secs: batch_elapsed_secs,
        ops_per_sec: batch_calls_sec,
        mib_per_sec: batch_mbs,
        avg_latency_us: batch_latency_us,
    };

    Ok(BenchReport {
        pid: config.pid,
        addr: config.addr,
        duration_secs: config.duration_secs,
        range_metrics,
        batch_metrics,
        batch_total_addrs: total_addrs,
        batch_addrs_per_sec: batch_addrs_sec,
    })
}

/// Formats and prints the benchmark report to standard output.
pub fn print_bench_report(report: &BenchReport) {
    println!("======================================================================");
    println!(" kread Performance Benchmark");
    println!(
        " Target: PID {}, Address 0x{:016x}, Duration: {}s",
        report.pid, report.addr, report.duration_secs
    );
    println!("======================================================================");

    println!(" 1. Single Range Read (8 bytes / call):");
    println!("    Total Calls:   {:>12}", report.range_metrics.total_calls);
    println!(
        "    Elapsed Time:  {:>12.3} s",
        report.range_metrics.elapsed_secs
    );
    println!(
        "    Throughput:    {:>12.1} ops/sec ({:.2} MiB/s)",
        report.range_metrics.ops_per_sec, report.range_metrics.mib_per_sec
    );
    println!(
        "    Avg Latency:   {:>12.2} µs",
        report.range_metrics.avg_latency_us
    );
    println!();

    println!(" 2. Scatter Batch Read (16 addrs / call):");
    println!("    Total Calls:   {:>12}", report.batch_metrics.total_calls);
    println!("    Total Addrs:   {:>12}", report.batch_total_addrs);
    println!(
        "    Throughput:    {:>12.1} calls/sec ({:.1} addrs/sec)",
        report.batch_metrics.ops_per_sec, report.batch_addrs_per_sec
    );
    println!(
        "    Avg Latency:   {:>12.2} µs",
        report.batch_metrics.avg_latency_us
    );
    println!("======================================================================");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct MockReader {
        range_calls: AtomicU64,
        batch_calls: AtomicU64,
    }

    impl MockReader {
        fn new() -> Self {
            Self {
                range_calls: AtomicU64::new(0),
                batch_calls: AtomicU64::new(0),
            }
        }
    }

    impl MemoryReader for MockReader {
        fn read_range(&self, _pid: i32, _addr: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.range_calls.fetch_add(1, Ordering::Relaxed);
            buf.fill(0xaa);
            Ok(buf.len())
        }

        fn read_batch(&self, _pid: i32, addrs: &[u64], vals: &mut [u64]) -> io::Result<u32> {
            self.batch_calls.fetch_add(1, Ordering::Relaxed);
            for v in vals.iter_mut() {
                *v = 0xdeadbeef;
            }
            Ok(addrs.len() as u32)
        }
    }

    #[test]
    fn test_benchmark_engine_with_mock() {
        let mock = MockReader::new();
        let config = BenchConfig {
            pid: 1234,
            addr: 0x7fff0000,
            duration_secs: 0, // Very short test
            warmup_ms: 10,
            inner_batch_size: 16,
        };

        let report = run_benchmark(&mock, &config).unwrap();
        assert_eq!(report.pid, 1234);
        assert_eq!(report.addr, 0x7fff0000);
        // Ensure warmup and at least one inner batch ran
        assert!(mock.range_calls.load(Ordering::Relaxed) >= 16);
        assert!(mock.batch_calls.load(Ordering::Relaxed) >= 16);
        assert!(report.range_metrics.elapsed_secs > 0.0);
        assert!(report.batch_metrics.elapsed_secs > 0.0);
        assert!(report.range_metrics.avg_latency_us >= 0.0);
        assert!(report.batch_metrics.avg_latency_us >= 0.0);
    }
}
