//! Command-line interface and background service daemon for `/dev/kread`.

use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::Instant;

use kread_daemon::bench::{print_bench_report, run_benchmark, BenchConfig};
use kread_daemon::driver::KreadDriver;
use kread_daemon::examples::{
    format_hexdump, format_utc_timestamp, is_daemon_process, parse_address, parse_length,
    DaemonStatusReport,
};
use kread_daemon::uapi::{KREAD_DEVICE_PATH, KREAD_MAX_RANGE_LEN};

static RUNNING: AtomicBool = AtomicBool::new(true);
static SIGNAL_PIPE_WRITE_FD: AtomicI32 = AtomicI32::new(-1);

extern "C" fn handle_signal(_sig: std::os::raw::c_int) {
    RUNNING.store(false, Ordering::SeqCst);
    let write_fd = SIGNAL_PIPE_WRITE_FD.load(Ordering::SeqCst);
    if write_fd >= 0 {
        // SAFETY:
        // `write` is guaranteed to be POSIX async-signal-safe. The write end of the self-pipe
        // was opened with O_NONBLOCK, ensuring this call never blocks inside the signal handler.
        let byte = [1u8];
        unsafe {
            write(write_fd, byte.as_ptr() as *const std::os::raw::c_void, 1);
        }
    }
}

/// C ABI `struct sigaction` layout matching Linux 64-bit libc (glibc and musl).
///
/// Total size: 152 bytes; alignment: 8 bytes.
/// - `sa_handler`: offset 0 (8 bytes)
/// - `sa_mask`: offset 8 (128 bytes, 16x u64)
/// - `sa_flags`: offset 136 (4 bytes, c_int)
/// - (padding): offset 140 (4 bytes)
/// - `sa_restorer`: offset 144 (8 bytes)
#[repr(C)]
struct SigAction {
    sa_handler: Option<extern "C" fn(std::os::raw::c_int)>,
    sa_mask: [u64; 16],
    sa_flags: std::os::raw::c_int,
    sa_restorer: Option<extern "C" fn()>,
}

const _: () = {
    assert!(std::mem::size_of::<SigAction>() == 152);
    assert!(std::mem::align_of::<SigAction>() == 8);
    assert!(std::mem::offset_of!(SigAction, sa_handler) == 0);
    assert!(std::mem::offset_of!(SigAction, sa_mask) == 8);
    assert!(std::mem::offset_of!(SigAction, sa_flags) == 136);
    assert!(std::mem::offset_of!(SigAction, sa_restorer) == 144);
};

#[repr(C)]
struct PollFd {
    fd: std::os::raw::c_int,
    events: std::os::raw::c_short,
    revents: std::os::raw::c_short,
}

const POLLIN: std::os::raw::c_short = 0x0001;
const O_CLOEXEC: std::os::raw::c_int = 0x80000;
const O_NONBLOCK: std::os::raw::c_int = 0x800;

unsafe extern "C" {
    fn sigaction(
        signum: std::os::raw::c_int,
        act: *const SigAction,
        oldact: *mut SigAction,
    ) -> std::os::raw::c_int;
    fn pipe2(pipefd: *mut std::os::raw::c_int, flags: std::os::raw::c_int) -> std::os::raw::c_int;
    fn poll(
        fds: *mut PollFd,
        nfds: std::os::raw::c_ulong,
        timeout: std::os::raw::c_int,
    ) -> std::os::raw::c_int;
    fn close(fd: std::os::raw::c_int) -> std::os::raw::c_int;
    fn write(
        fd: std::os::raw::c_int,
        buf: *const std::os::raw::c_void,
        count: usize,
    ) -> isize;
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

/// Resolves the secure runtime directory for PID, lock, and log files.
///
/// Priority:
/// 1. Explicit environment variable `KREAD_RUNTIME_DIR` (if set and valid directory).
/// 2. KernelSU module directory `/data/adb/modules/kread` (if it exists and is a directory).
/// 3. Secure root-owned fallback directory with mode 0700:
///    - On Android: `/data/local/tmp/kread_secure`
///    - On host/Linux: `/tmp/kread_secure`
pub fn determine_runtime_dir() -> PathBuf {
    if let Ok(env_dir) = env::var("KREAD_RUNTIME_DIR") {
        let p = PathBuf::from(env_dir);
        if p.is_dir() {
            return p;
        }
    }

    let ksu_dir = Path::new("/data/adb/modules/kread");
    if ksu_dir.is_dir() {
        return ksu_dir.to_path_buf();
    }

    // Secure fallback: create private directory with permissions 0700
    let candidate = if Path::new("/data/local/tmp").is_dir() {
        PathBuf::from("/data/local/tmp/kread_secure")
    } else {
        PathBuf::from("/tmp/kread_secure")
    };

    if let Ok(meta) = fs::symlink_metadata(&candidate) {
        if meta.file_type().is_symlink() {
            let _ = fs::remove_file(&candidate);
            let _ = fs::create_dir_all(&candidate);
        }
    } else {
        let _ = fs::create_dir_all(&candidate);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700));
    }

    candidate
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
        return Err(format!(
            "Requested length exceeds maximum ({} MB)",
            KREAD_MAX_RANGE_LEN / (1024 * 1024)
        )
        .into());
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
    println!(
        " {:<6} | {:<18} | {:<18}",
        "Index", "Virtual Address", "Value (u64)"
    );
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

    let config = BenchConfig {
        pid,
        addr,
        duration_secs,
        warmup_ms: 100,
        inner_batch_size: 128,
    };

    let report = run_benchmark(&driver, &config)?;
    print_bench_report(&report);

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

    // Create self-pipe for interruptible signal waiting
    let mut pipe_fds = [-1i32; 2];
    let pipe_ret = unsafe {
        // SAFETY: pipe2 receives a valid pointer to a stack-allocated 2-element c_int array.
        // O_CLOEXEC prevents fd leak across exec; O_NONBLOCK ensures async-signal-safe writes.
        pipe2(pipe_fds.as_mut_ptr(), O_CLOEXEC | O_NONBLOCK)
    };
    if pipe_ret < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let (read_fd, write_fd) = (pipe_fds[0], pipe_fds[1]);
    SIGNAL_PIPE_WRITE_FD.store(write_fd, Ordering::SeqCst);

    // Install POSIX sigaction signal handlers for SIGINT (2) and SIGTERM (15)
    let mut sa: SigAction = unsafe { std::mem::zeroed() };
    sa.sa_handler = Some(handle_signal);
    sa.sa_flags = 0; // Explicitly 0: no SA_RESTART so syscalls interrupt promptly

    unsafe {
        // SAFETY:
        // 1. AAPCS64 & C ABI: `sigaction` expects integer signum in reg 0, `*const SigAction` in reg 1,
        //    and null pointer in reg 2.
        // 2. Struct Invariant: `sa` is zeroed, setting `sa_mask` to empty (no blocked signals) and `sa_flags`
        //    to 0. `handle_signal` is a valid extern "C" fn using strictly async-signal-safe operations.
        // 3. Lifetime: `sa` is on the stack and valid for the duration of the synchronous call.
        sigaction(2, &sa, std::ptr::null_mut());
        sigaction(15, &sa, std::ptr::null_mut());
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
        "[{}] [INFO] kread_daemon v{} started (pid: {}, arch: {}, interval: {}s)\n",
        format_utc_timestamp(),
        env!("CARGO_PKG_VERSION"),
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

        // Battery-friendly sleep: sleep in deep kernel suspend state via poll() on the self-pipe
        // for up to interval_sec * 1000 ms. Immediately returns if a signal arrives.
        let mut pfd = PollFd {
            fd: read_fd,
            events: POLLIN,
            revents: 0,
        };
        let timeout_ms = (interval_sec.min(3600) * 1000) as std::os::raw::c_int;

        unsafe {
            // SAFETY: poll receives a valid stack pointer to PollFd and descriptor count 1.
            poll(&mut pfd, 1, timeout_ms);
        }
    }

    // Clean up self-pipe
    SIGNAL_PIPE_WRITE_FD.store(-1, Ordering::SeqCst);
    unsafe {
        // SAFETY: Closing valid file descriptors returned by pipe2.
        close(read_fd);
        close(write_fd);
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

    if let Ok(content) = fs::read_to_string(&pid_path)
        && let Ok(pid) = content.trim().parse::<u32>()
    {
        daemon_pid = Some(pid);
        daemon_running = is_daemon_process(pid);
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
            println!(
                "kread_daemon v{} (ARM64 GKI Modern Edition)",
                env!("CARGO_PKG_VERSION")
            );
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_determine_runtime_dir() {
        let dir = determine_runtime_dir();
        assert!(
            dir.starts_with("/data/local/tmp")
                || dir.starts_with("/tmp")
                || dir.starts_with("/data/adb/modules/kread")
        );
    }

    #[test]
    fn test_sigaction_abi_layout() {
        assert_eq!(std::mem::size_of::<SigAction>(), 152);
        assert_eq!(std::mem::align_of::<SigAction>(), 8);
        assert_eq!(std::mem::offset_of!(SigAction, sa_handler), 0);
        assert_eq!(std::mem::offset_of!(SigAction, sa_mask), 8);
        assert_eq!(std::mem::offset_of!(SigAction, sa_flags), 136);
        assert_eq!(std::mem::offset_of!(SigAction, sa_restorer), 144);
    }
}

