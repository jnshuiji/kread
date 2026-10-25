//! RFC 8259 compliant JSON serialization and daemon health status reporting.

use std::fmt::Write as FmtWrite;

/// Encodes an arbitrary string slice into RFC 8259 compliant JSON string literal content.
///
/// Escapes quotation marks (`"`), reverse solidi (`\`), standard control characters
/// (`\n`, `\r`, `\t`, `\b`, `\f`), and all ASCII control characters U+0000 through U+001F
/// as unicode escapes (`\u00xx`).
pub fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

/// Status payload for KernelSU WebUI / Action reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonStatusReport {
    /// Whether the device node `/dev/kread` exists in devtmpfs.
    pub device_present: bool,
    /// Absolute path to the character device node.
    pub device_path: String,
    /// Whether opening the device node with read/write access succeeded.
    pub driver_accessible: bool,
    /// Error message if opening the driver failed (null if successful).
    pub driver_error: Option<String>,
    /// Process ID of the running background daemon (null if not running).
    pub daemon_pid: Option<u32>,
    /// Whether the background daemon process is verified alive.
    pub daemon_running: bool,
    /// Filesystem path to the active daemon log file (null if absent).
    pub daemon_log_path: Option<String>,
    /// Package version string from Cargo metadata.
    pub version: String,
    /// Target architecture string (`aarch64` or `x86_64`).
    pub target_arch: String,
    /// High-level operational status (`ready`, `device_missing`, `permission_denied`, etc.).
    pub status: String,
}

impl DaemonStatusReport {
    /// Serializes the status report into RFC 8259 compliant JSON.
    ///
    /// Every string field is properly escaped through [`escape_json`], guaranteeing
    /// that malformed OS error messages or abnormal paths cannot produce invalid JSON.
    pub fn to_json(&self) -> String {
        let err_json = match &self.driver_error {
            Some(e) => format!("\"{}\"", escape_json(e)),
            None => "null".to_string(),
        };
        let pid_json = match self.daemon_pid {
            Some(p) => p.to_string(),
            None => "null".to_string(),
        };
        let log_json = match &self.daemon_log_path {
            Some(l) => format!("\"{}\"", escape_json(l)),
            None => "null".to_string(),
        };

        format!(
            "{{\n  \"device_present\": {},\n  \"device_path\": \"{}\",\n  \"driver_accessible\": {},\n  \"driver_error\": {},\n  \"daemon_pid\": {},\n  \"daemon_running\": {},\n  \"daemon_log_path\": {},\n  \"version\": \"{}\",\n  \"target_arch\": \"{}\",\n  \"status\": \"{}\"\n}}",
            self.device_present,
            escape_json(&self.device_path),
            self.driver_accessible,
            err_json,
            pid_json,
            self.daemon_running,
            log_json,
            escape_json(&self.version),
            escape_json(&self.target_arch),
            escape_json(&self.status)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_escape_json_basic() {
        assert_eq!(escape_json("simple"), "simple");
        assert_eq!(escape_json(r#"quote: """#), r#"quote: \"\""#);
        assert_eq!(escape_json(r#"backslash: \"#), r#"backslash: \\"#);
        assert_eq!(escape_json("newline:\nand\rcarriage"), r#"newline:\nand\rcarriage"#);
        assert_eq!(escape_json("tab:\tand\x08backspace"), r#"tab:\tand\bbackspace"#);
    }

    #[test]
    fn test_escape_json_control_chars() {
        let control = "\x00\x01\x1f";
        assert_eq!(escape_json(control), r#"\u0000\u0001\u001f"#);
    }

    #[test]
    fn test_status_report_to_json_validity() {
        let report = DaemonStatusReport {
            device_present: false,
            device_path: "/dev/kread \"special\" \\ path".to_string(),
            driver_accessible: false,
            driver_error: Some("Failed to open /dev/kread:\nPermission denied \\ EACCES\t(code 13)\0".to_string()),
            daemon_pid: Some(42),
            daemon_running: true,
            daemon_log_path: Some("/data/local/tmp/kread \"daemon\".log".to_string()),
            version: "1.0.0".to_string(),
            target_arch: "aarch64".to_string(),
            status: "error\nstate".to_string(),
        };

        let json = report.to_json();
        // Verify key substrings are escaped
        assert!(json.contains(r#"/dev/kread \"special\" \\ path"#));
        assert!(json.contains(r#"Failed to open /dev/kread:\nPermission denied \\ EACCES\t(code 13)\u0000"#));
        assert!(json.contains(r#"/data/local/tmp/kread \"daemon\".log"#));
        assert!(json.contains("\"status\": \"error\\nstate\""));
        assert!(json.contains("\"daemon_pid\": 42"));
        assert!(json.contains("\"device_present\": false"));
    }
}
