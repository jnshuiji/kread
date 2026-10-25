//! Demonstration routines, formatting helpers, and diagnostics for kread.

use std::fmt::Write as FmtWrite;
use std::time::SystemTime;

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

extern "C" {
    fn kill(pid: std::os::raw::c_int, sig: std::os::raw::c_int) -> std::os::raw::c_int;
}

/// Checks whether a given process ID is currently running on the system.
pub fn is_pid_alive(pid: u32) -> bool {
    if std::path::Path::new(&format!("/proc/{}", pid)).exists() {
        return true;
    }
    unsafe { kill(pid as std::os::raw::c_int, 0) == 0 }
}

/// Status payload for KernelSU WebUI / Action reporting.
#[derive(Debug, Clone)]
pub struct DaemonStatusReport {
    pub device_present: bool,
    pub device_path: String,
    pub driver_accessible: bool,
    pub driver_error: Option<String>,
    pub daemon_pid: Option<u32>,
    pub daemon_running: bool,
    pub daemon_log_path: Option<String>,
    pub version: String,
    pub target_arch: String,
    pub status: String,
}

impl DaemonStatusReport {
    /// Formats the report as valid JSON for easy consumption by scripts.
    pub fn to_json(&self) -> String {
        let err_json = match &self.driver_error {
            Some(e) => format!("\"{}\"", e.replace('\"', "\\\"")),
            None => "null".to_string(),
        };
        let pid_json = match self.daemon_pid {
            Some(p) => p.to_string(),
            None => "null".to_string(),
        };
        let log_json = match &self.daemon_log_path {
            Some(l) => format!("\"{}\"", l.replace('\"', "\\\"")),
            None => "null".to_string(),
        };

        format!(
            "{{\n  \"device_present\": {},\n  \"device_path\": \"{}\",\n  \"driver_accessible\": {},\n  \"driver_error\": {},\n  \"daemon_pid\": {},\n  \"daemon_running\": {},\n  \"daemon_log_path\": {},\n  \"version\": \"{}\",\n  \"target_arch\": \"{}\",\n  \"status\": \"{}\"\n}}",
            self.device_present,
            self.device_path,
            self.driver_accessible,
            err_json,
            pid_json,
            self.daemon_running,
            log_json,
            self.version,
            self.target_arch,
            self.status
        )
    }
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
}
