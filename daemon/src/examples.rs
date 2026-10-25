//! Demonstration routines, formatting helpers, and diagnostics for kread.

use std::fmt::Write as FmtWrite;
use std::time::SystemTime;

pub use crate::status::{escape_json, DaemonStatusReport};

/// Formats a byte slice as a canonical hexdump (16 bytes per line with hex offset and ASCII representation).
pub fn format_hexdump(data: &[u8], base_addr: u64) -> String {
    if data.is_empty() {
        return "<empty data>\n".to_string();
    }

    let mut out = String::new();
    let chunk_size = 16;

    for (i, chunk) in data.chunks(chunk_size).enumerate() {
        let offset = base_addr + (i * chunk_size) as u64;
        let _ = write!(out, "{:08x}: ", offset);

        // Hex bytes
        for j in 0..chunk_size {
            if j < chunk.len() {
                let _ = write!(out, "{:02x} ", chunk[j]);
            } else {
                out.push_str("   ");
            }
            if j == 7 {
                out.push(' ');
            }
        }

        out.push_str(" |");
        // ASCII representation
        for &b in chunk {
            if (0x20..=0x7e).contains(&b) {
                out.push(b as char);
            } else {
                out.push('.');
            }
        }
        out.push_str("|\n");
    }

    out
}

/// Parses an address string supporting hex (`0x...` or `...h`) and decimal.
pub fn parse_address(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty address string".to_string());
    }

    if let Some(stripped) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(stripped, 16)
            .map_err(|e| format!("invalid hex address '{}': {}", s, e))
    } else if let Ok(val) = s.parse::<u64>() {
        Ok(val)
    } else {
        u64::from_str_radix(s, 16)
            .map_err(|e| format!("invalid address '{}': {}", s, e))
    }
}

/// Parses a length string supporting hex (`0x...`) and decimal.
pub fn parse_length(s: &str) -> Result<usize, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("empty length string".to_string());
    }

    if let Some(stripped) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        usize::from_str_radix(stripped, 16)
            .map_err(|e| format!("invalid hex length '{}': {}", s, e))
    } else {
        s.parse::<usize>()
            .map_err(|e| format!("invalid length '{}': {}", s, e))
    }
}

/// Formats current UTC timestamp in ISO-8601 format (`YYYY-MM-DDTHH:MM:SSZ`) without external crates.
pub fn format_utc_timestamp() -> String {
    let now = SystemTime::now();
    let duration = now.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let total_secs = duration.as_secs();

    let secs_of_day = total_secs % 86400;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    let mut days = total_secs / 86400;
    let mut year = 1970;
    loop {
        let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_year = if leap { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
    let days_in_months = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for &dim in &days_in_months {
        if days < dim {
            break;
        }
        days -= dim;
        month += 1;
    }
    let day = days + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hour, minute, second
    )
}

/// Maximum architectural PID limit supported on 64-bit Linux kernels (4,194,304).
pub const PID_MAX_LIMIT: u32 = 4_194_304;

unsafe extern "C" {
    fn kill(pid: std::os::raw::c_int, sig: std::os::raw::c_int) -> std::os::raw::c_int;
}

/// Checks whether a given process ID is currently running on the system.
///
/// Strictly bounds the PID within valid Linux kernel PID limits (`1..=PID_MAX_LIMIT`),
/// eliminating false positives from `kill(0, 0)` (process group) and `kill(-1, 0)` (all processes).
/// Accurately handles unprivileged callers where `kill(pid, 0)` returns `-1` with `errno == EPERM`.
pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > PID_MAX_LIMIT {
        return false;
    }
    if std::path::Path::new(&format!("/proc/{}", pid)).exists() {
        return true;
    }
    // SAFETY:
    // Calling `kill(pid, 0)` with signal 0 performs permission and existence checking
    // without delivering a signal. With 1 <= pid <= PID_MAX_LIMIT, `pid as c_int` is strictly
    // positive, guaranteeing POSIX individual process signaling rather than process group signaling.
    let ret = unsafe { kill(pid as std::os::raw::c_int, 0) };
    if ret == 0 {
        return true;
    }
    let err = std::io::Error::last_os_error();
    // EPERM (raw OS error 1 on Linux): process exists but caller lacks privilege to signal it.
    err.raw_os_error() == Some(1)
}

/// Verifies whether the process with `pid` is running and matches the daemon binary name.
///
/// Hardened against:
/// 1. Zombie / defunct processes (checked via `/proc/<pid>/status` state != 'Z' / 'X').
/// 2. Argument substring spoofing / false positives (verifies binary via `/proc/<pid>/exe`
///    or `argv[0]` rather than substring search across `/proc/<pid>/cmdline`).
pub fn is_daemon_process(pid: u32) -> bool {
    if !is_pid_alive(pid) {
        return false;
    }

    // 1. Zombie / Dead Process Detection via /proc/<pid>/status
    if let Ok(status) = std::fs::read_to_string(format!("/proc/{}/status", pid)) {
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("State:") {
                let state = rest.trim_start();
                if state.starts_with('Z') || state.starts_with('X') {
                    return false;
                }
                break;
            }
        }
    }

    // 2. Primary Identity: /proc/<pid>/exe symlink
    if let Ok(exe_path) = std::fs::read_link(format!("/proc/{}/exe", pid))
        && let Some(file_name) = exe_path.file_name().and_then(|n| n.to_str())
    {
        let clean_name = file_name.strip_suffix(" (deleted)").unwrap_or(file_name);
        return clean_name == "kread_daemon";
    }

    // 3. Fallback Identity: /proc/<pid>/comm (maximum 16 bytes on Linux)
    let comm_matches = std::fs::read_to_string(format!("/proc/{}/comm", pid))
        .map(|comm| comm.trim() == "kread_daemon")
        .unwrap_or(false);

    if !comm_matches {
        return false;
    }

    // 4. Cross-Verification: inspect argv[0] from /proc/<pid>/cmdline
    if let Ok(cmdline) = std::fs::read(format!("/proc/{}/cmdline", pid))
        && !cmdline.is_empty()
    {
        let argv0_bytes = cmdline.split(|&b| b == 0).next().unwrap_or(&[]);
        if let Ok(argv0) = std::str::from_utf8(argv0_bytes) {
            let argv0_base = std::path::Path::new(argv0)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(argv0);
            let clean_base = argv0_base.strip_suffix(" (deleted)").unwrap_or(argv0_base);
            return clean_base == "kread_daemon";
        }
    }

    // If cmdline was empty or inaccessible but comm matched, accept
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_hexdump() {
        let bytes = b"Hello, World!\x00\x01\x02";
        let dump = format_hexdump(bytes, 0x1000);
        assert!(dump.contains("00001000: 48 65 6c 6c 6f 2c 20 57"));
        assert!(dump.contains("|Hello, World!...|"));
    }

    #[test]
    fn test_parse_address() {
        assert_eq!(parse_address("0x1234").unwrap(), 0x1234);
        assert_eq!(parse_address("0X7FFFFFFF").unwrap(), 0x7fffffff);
        assert_eq!(parse_address("4096").unwrap(), 4096);
        assert_eq!(parse_address("deadbeef").unwrap(), 0xdeadbeef);
    }

    #[test]
    fn test_parse_length() {
        assert_eq!(parse_length("64").unwrap(), 64);
        assert_eq!(parse_length("0x40").unwrap(), 64);
    }

    #[test]
    fn test_format_utc_timestamp() {
        let ts = format_utc_timestamp();
        assert_eq!(ts.len(), 20);
        assert!(ts.ends_with('Z'));
        assert!(ts.contains('T'));
    }

    #[test]
    fn test_status_json() {
        let rep = DaemonStatusReport {
            device_present: true,
            device_path: "/dev/kread".to_string(),
            driver_accessible: true,
            driver_error: None,
            daemon_pid: Some(999),
            daemon_running: true,
            daemon_log_path: Some("/data/local/tmp/kread_daemon.log".to_string()),
            version: "2.0.0".to_string(),
            target_arch: "aarch64".to_string(),
            status: "ready".to_string(),
        };
        let j = rep.to_json();
        assert!(j.contains("\"status\": \"ready\""));
        assert!(j.contains("\"daemon_pid\": 999"));
        assert!(j.contains("\"daemon_running\": true"));
    }

    #[test]
    fn test_is_pid_alive_self() {
        let my_pid = std::process::id();
        assert!(is_pid_alive(my_pid));
    }

    #[test]
    fn test_is_pid_alive_nonexistent() {
        // PID 4194304 is usually beyond default pid_max (4194304) or unlikely to exist
        assert!(!is_pid_alive(4_194_300));
    }

    #[test]
    fn test_is_pid_alive_bounds() {
        assert!(!is_pid_alive(0));
        assert!(!is_pid_alive(u32::MAX));
        assert!(!is_pid_alive(i32::MAX as u32));
        assert!(!is_pid_alive(PID_MAX_LIMIT + 1));
    }

    #[test]
    fn test_is_daemon_process_rejection() {
        assert!(!is_daemon_process(0));
        assert!(!is_daemon_process(u32::MAX));
        assert!(!is_daemon_process(PID_MAX_LIMIT + 1));
        assert!(!is_daemon_process(std::process::id()));
    }
}
