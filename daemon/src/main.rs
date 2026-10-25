//! Command-line interface and background service daemon for `/dev/kread`.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use kread_daemon::driver::KreadDriver;
use kread_daemon::examples::{
    format_hexdump, format_utc_timestamp, is_pid_alive, parse_address, parse_length,
    DaemonStatusReport,
};
use kread_daemon::uapi::{KREAD_DEVICE_PATH, KREAD_MAX_RANGE_LEN};

static RUNNING: AtomicBool = AtomicBool::new(true);

extern "C" fn handle_signal(_sig: std::os::raw::c_int) {
    RUNNING.store(false, Ordering::SeqCst);
}

unsafe extern "C" {
    fn signal(signum: std::os::raw::c_int, handler: extern "C" fn(std::os::raw::c_int)) -> usize;
}

fn print_usage() {
    eprintln!(
        concat!(
            "kread_daemon v", env!("CARGO_PKG_VERSION"), " — Production Rust Daemon & CLI for /dev/kread\n\n",
            "USAGE:\n",
            "    kread_daemon <SUBCOMMAND> [OPTIONS]\n\n",
            "SUBCOMMANDS:\n",
            "    range <pid> <addr> <len>      Read virtual memory range with formatted hex + ASCII dump\n",
            "    batch <pid> <addr1> [addrs..] Batch scatter read 64-bit values across target process\n",
            "    bench <pid> <addr> [duration] Run throughput & latency performance benchmark (default: 3s)\n",
            "    daemon [--interval <sec>]     Run background monitoring service (logs to /data/local/tmp)\n",
            "    status                        Check driver and daemon health (prints JSON for KernelSU)\n",
            "    help, --help                  Print this help message\n",
            "    version, --version            Print version information\n\n",
            "EXAMPLES:\n",
            "    kread_daemon range 1337 0x7ffffff000 64\n",
            "    kread_daemon batch 1337 0x7ffffff000 0x7ffffff008 0x7ffffff010\n",
            "    kread_daemon bench 1337 0x7ffffff000 5\n",
            "    kread_daemon daemon\n",
            "    kread_daemon status\n"
        )
    );
}

fn determine_runtime_dir() -> PathBuf {
    let android_tmp = Path::new("/data/local/tmp");
    if android_tmp.is_dir() {
        android_tmp.to_path_buf()
    } else {
        PathBuf::from("/tmp")
    }
}

fn cmd_range(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 3 {
        eprintln!("Error: 'range' requires <pid> <addr> <len>");
        eprintln!("Usage: kread_daemon range <pid> <addr> <len>");
        process::exit(1);
    }

    let pid: i32 = args[0]
        .parse()
        .map_err(|e| format!("Invalid pid '{}': {}", args[0], e))?;
    let addr = parse_address(&args[1])?;
    let len = parse_length(&args[2])?;

    if len > KREAD_MAX_RANGE_LEN {
        return Err(format!("Requested length exceeds maximum ({} MB)", KREAD_MAX_RANGE_LEN / (1024 * 1024)).into());
    }

    let driver = KreadDriver::open().map_err(|e| {
        format!(
            "Failed to open {} ({}): ensure kread.ko is loaded and permissions are 0666",
            KREAD_DEVICE_PATH, e
        )
    })?;

    println!(
        "[kread_daemon] Reading {} bytes from PID {} at 0x{:016x} via /dev/kread...",
        len, pid, addr
    );

    let start = Instant::now();
    let data = driver.read_range(pid, addr, len)?;
    let elapsed = start.elapsed();

    print!("{}", format_hexdump(&data, addr));
    println!(
        "[kread_daemon] Transferred {}/{} bytes in {:.2?} ({:.2} MB/s)",
        data.len(),
        len,
        elapsed,
        (data.len() as f64 / 1_048_576.0) / elapsed.as_secs_f64().max(0.000001)
    );

    Ok(())
}

fn cmd_batch(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        eprintln!("Error: 'batch' requires <pid> <addr1> [addr2...]");
        eprintln!("Usage: kread_daemon batch <pid> <addr1> [addr2...]");
        process::exit(1);
    }

    let pid: i32 = args[0]
        .parse()
        .map_err(|e| format!("Invalid pid '{}': {}", args[0], e))?;

    let mut addrs = Vec::with_capacity(args.len() - 1);
    for arg in &args[1..] {
        addrs.push(parse_address(arg)?);
    }

    let driver = KreadDriver::open().map_err(|e| {
        format!(
            "Failed to open {} ({}): ensure kread.ko is loaded and permissions are 0666",
            KREAD_DEVICE_PATH, e
        )
    })?;

    println!(
        "[kread_daemon] Batch reading {} addresses from PID {} via /dev/kread...",
        addrs.len(),
        pid
    );

    let start = Instant::now();
    let (vals, success_count) = driver.read_batch(pid, &addrs)?;
    let elapsed = start.elapsed();

    println!("{:-<68}", "");
    println!(" {:<6} | {:<18} | {:<18}", "Index", "Virtual Address", "Value (u64)");
    println!("{:-<68}", "");
    for (i, (&addr, &val)) in addrs.iter().zip(vals.iter()).enumerate() {
        println!(" [{:04}] | 0x{:016x} | 0x{:016x}", i, addr, val);
    }
    println!("{:-<68}", "");
    println!(
        "[kread_daemon] Success: {}/{} resident entries resolved in {:.2?}",
        success_count,
        addrs.len(),
        elapsed
    );

    Ok(())
}

fn cmd_bench(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    if args.len() < 2 {
        eprintln!("Error: 'bench' requires <pid> <addr> [duration_sec]");
        eprintln!("Usage: kread_daemon bench <pid> <addr> [duration_sec]");
        process::exit(1);
    }

    let pid: i32 = args[0]
        .parse()
        .map_err(|e| format!("Invalid pid '{}': {}", args[0], e))?;
    let addr = parse_address(&args[1])?;
    let duration_secs: u64 = if args.len() >= 3 {
        args[2]
            .parse()
            .map_err(|e| format!("Invalid duration '{}': {}", args[2], e))?
    } else {
        3
    };

    let driver = KreadDriver::open().map_err(|e| {
        format!(
            "Failed to open {} ({}): ensure kread.ko is loaded and permissions are 0666",
            KREAD_DEVICE_PATH, e
        )
    })?;

    println!("======================================================================");
    println!(" kread Performance Benchmark");
    println!(" Target: PID {}, Address 0x{:016x}, Duration: {}s", pid, addr, duration_secs);
    println!("======================================================================");

    // 1. Single 8-byte range read benchmark
    let bench_duration = Duration::from_secs(duration_secs);
    let mut buf = [0u8; 8];
    let mut range_iters = 0u64;
    let range_start = Instant::now();

    while range_start.elapsed() < bench_duration {
        let _ = driver.read_range_into(pid, addr, &mut buf)?;
        range_iters += 1;
    }
    let range_elapsed = range_start.elapsed();
    let range_ops = range_iters as f64 / range_elapsed.as_secs_f64();
    let range_bytes = range_iters * 8;
    let range_mbs = (range_bytes as f64 / 1_048_576.0) / range_elapsed.as_secs_f64();
    let range_latency_us = (range_elapsed.as_micros() as f64) / range_iters as f64;

    println!(" 1. Single Range Read (8 bytes / call):");
    println!("    Total Calls:   {:>12}", range_iters);
    println!("    Elapsed Time:  {:>12.3} s", range_elapsed.as_secs_f64());
    println!("    Throughput:    {:>12.1} ops/sec ({:.2} MiB/s)", range_ops, range_mbs);
    println!("    Avg Latency:   {:>12.2} µs", range_latency_us);
    println!();

    // 2. Batch read benchmark (16 addresses)
    let batch_addrs: Vec<u64> = (0..16).map(|i| addr.wrapping_add(i * 8)).collect();
    let mut batch_iters = 0u64;
    let batch_start = Instant::now();

    while batch_start.elapsed() < bench_duration {
        let _ = driver.read_batch(pid, &batch_addrs)?;
        batch_iters += 1;
    }
    let batch_elapsed = batch_start.elapsed();
    let batch_calls_sec = batch_iters as f64 / batch_elapsed.as_secs_f64();
    let batch_addrs_sec = (batch_iters * 16) as f64 / batch_elapsed.as_secs_f64();
    let batch_latency_us = (batch_elapsed.as_micros() as f64) / batch_iters as f64;

    println!(" 2. Scatter Batch Read (16 addrs / call):");
    println!("    Total Calls:   {:>12}", batch_iters);
    println!("    Total Addrs:   {:>12}", batch_iters * 16);
    println!("    Throughput:    {:>12.1} calls/sec ({:.1} addrs/sec)", batch_calls_sec, batch_addrs_sec);
    println!("    Avg Latency:   {:>12.2} µs", batch_latency_us);
    println!("======================================================================");

    Ok(())
}

fn cmd_daemon(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let mut interval_sec = 5u64;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--interval" && i + 1 < args.len() {
            interval_sec = args[i + 1].parse().unwrap_or(5).max(1);
            i += 2;
        } else {
            i += 1;
        }
    }

    let runtime_dir = determine_runtime_dir();
    let log_path = runtime_dir.join("kread_daemon.log");
    let pid_path = runtime_dir.join("kread_daemon.pid");

    let current_pid = process::id();
    fs::write(&pid_path, current_pid.to_string())?;

    // Install signal handlers for SIGINT (2) and SIGTERM (15)
    unsafe {
        signal(2, handle_signal);
        signal(15, handle_signal);
    }

    let mut log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;

    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };

    let start_msg = format!(
        "[{}] [INFO] kread_daemon v2.0.0 started (pid: {}, arch: {}, interval: {}s)\n",
        format_utc_timestamp(),
        current_pid,
        arch,
        interval_sec
    );
    let _ = log_file.write_all(start_msg.as_bytes());
    let _ = log_file.flush();
    println!("{}", start_msg.trim_end());

    let start_time = Instant::now();

    while RUNNING.load(Ordering::SeqCst) {
        let uptime = start_time.elapsed().as_secs();
        let status = match KreadDriver::open() {
            Ok(driver) => {
                if driver.is_alive() {
                    "OK"
                } else {
                    "FD_INVALID"
                }
            }
            Err(_e) => {
                if !Path::new(KREAD_DEVICE_PATH).exists() {
                    "DEV_MISSING"
                } else {
                    "ACCESS_DENIED"
                }
            }
        };

        let line = format!(
            "[{}] [INFO] HEARTBEAT | driver: {} | dev: {} | uptime: {}s | pid: {}\n",
            format_utc_timestamp(),
            status,
            KREAD_DEVICE_PATH,
            uptime,
            current_pid
        );
        let _ = log_file.write_all(line.as_bytes());
        let _ = log_file.flush();

        // Sleep with 500ms intervals to remain responsive to signals
        let sleep_until = Instant::now() + Duration::from_secs(interval_sec);
        while RUNNING.load(Ordering::SeqCst) && Instant::now() < sleep_until {
            thread::sleep(Duration::from_millis(250));
        }
    }

    let stop_msg = format!(
        "[{}] [INFO] kread_daemon shutting down gracefully\n",
        format_utc_timestamp()
    );
    let _ = log_file.write_all(stop_msg.as_bytes());
    let _ = log_file.flush();
    let _ = fs::remove_file(&pid_path);
    println!("{}", stop_msg.trim_end());

    Ok(())
}

fn cmd_status() -> Result<(), Box<dyn std::error::Error>> {
    let runtime_dir = determine_runtime_dir();
    let pid_path = runtime_dir.join("kread_daemon.pid");
    let log_path = runtime_dir.join("kread_daemon.log");

    let device_present = Path::new(KREAD_DEVICE_PATH).exists();
    let (driver_accessible, driver_error) = match KreadDriver::open() {
        Ok(_) => (true, None),
        Err(e) => (false, Some(e.to_string())),
    };

    let mut daemon_pid = None;
    let mut daemon_running = false;

    if let Ok(content) = fs::read_to_string(&pid_path) {
        if let Ok(pid) = content.trim().parse::<u32>() {
            daemon_pid = Some(pid);
            daemon_running = is_pid_alive(pid);
        }
    }

    let status = if driver_accessible {
        "ready"
    } else if !device_present {
        "device_missing"
    } else {
        "permission_denied"
    };

    let report = DaemonStatusReport {
        device_present,
        device_path: KREAD_DEVICE_PATH.to_string(),
        driver_accessible,
        driver_error,
        daemon_pid,
        daemon_running,
        daemon_log_path: if log_path.exists() {
            Some(log_path.to_string_lossy().to_string())
        } else {
            None
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
        target_arch: if cfg!(target_arch = "aarch64") {
            "aarch64".to_string()
        } else {
            "x86_64".to_string()
        },
        status: status.to_string(),
    };

    println!("{}", report.to_json());
    Ok(())
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        process::exit(1);
    }

    let subcommand = args[1].as_str();
    let sub_args = &args[2..];

    let result = match subcommand {
        "range" => cmd_range(sub_args),
        "batch" => cmd_batch(sub_args),
        "bench" => cmd_bench(sub_args),
        "daemon" => cmd_daemon(sub_args),
        "status" => cmd_status(),
        "help" | "--help" | "-h" => {
            print_usage();
            Ok(())
        }
        "version" | "--version" | "-v" => {
            println!("kread_daemon v{} (ARM64 GKI Modern Edition)", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        unknown => {
            eprintln!("Unknown subcommand: '{}'", unknown);
            print_usage();
            process::exit(1);
        }
    };

    if let Err(err) = result {
        eprintln!("[kread_daemon error] {}", err);
        process::exit(1);
    }
}
