//! Empirical stress-testing suite for Challenger 2 (Milestone M1 Iteration 2).
//!
//! Covers:
//! 1. `is_pid_alive` empirical boundary testing (0, 1, 4194304, 4194305, u32::MAX, negative numbers).
//! 2. `is_daemon_process` empirical resistance to zombies and argument spoofing.
//! 3. Signal delivery under unified 152B `SigAction` struct on x86_64 and aarch64.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use kread_daemon::examples::{is_daemon_process, is_pid_alive, PID_MAX_LIMIT};

unsafe extern "C" {
    fn kill(pid: std::os::raw::c_int, sig: std::os::raw::c_int) -> std::os::raw::c_int;
    fn fork() -> std::os::raw::c_int;
    fn waitpid(
        pid: std::os::raw::c_int,
        status: *mut std::os::raw::c_int,
        options: std::os::raw::c_int,
    ) -> std::os::raw::c_int;
    fn prctl(
        option: std::os::raw::c_int,
        arg2: usize,
        arg3: usize,
        arg4: usize,
        arg5: usize,
    ) -> std::os::raw::c_int;
    fn _exit(status: std::os::raw::c_int) -> !;
}

const PR_SET_NAME: std::os::raw::c_int = 15;

#[repr(C)]
struct SigAction {
    sa_handler: Option<extern "C" fn(std::os::raw::c_int)>,
    sa_mask: [u64; 16],
    sa_flags: std::os::raw::c_int,
    sa_restorer: Option<extern "C" fn()>,
}

unsafe extern "C" {
    fn sigaction(
        signum: std::os::raw::c_int,
        act: *const SigAction,
        oldact: *mut SigAction,
    ) -> std::os::raw::c_int;
}

static TEST_SIGNAL_RECEIVED: AtomicU32 = AtomicU32::new(0);

extern "C" fn test_handler(sig: std::os::raw::c_int) {
    TEST_SIGNAL_RECEIVED.store(sig as u32, Ordering::SeqCst);
}

fn locate_kread_daemon_bin(target_arch: &str) -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let cand = if target_arch == "aarch64" {
        manifest_dir
            .join("target")
            .join("aarch64-unknown-linux-musl")
            .join("release")
            .join("kread_daemon")
    } else {
        manifest_dir
            .join("target")
            .join("release")
            .join("kread_daemon")
    };

    if cand.is_file() {
        return cand;
    }

    // Fallback to debug
    let debug_cand = manifest_dir.join("target").join("debug").join("kread_daemon");
    if debug_cand.is_file() {
        return debug_cand;
    }

    panic!("Could not locate kread_daemon binary at {:?}", cand);
}

// ==============================================================================
// 1. is_pid_alive Empirical Tests
// ==============================================================================

#[test]
fn test_empirical_is_pid_alive_boundaries() {
    println!("[TEST] Verifying is_pid_alive empirical boundaries...");

    // 1. pid = 0 MUST be rejected (kill(0, 0) would signal process group)
    assert!(
        !is_pid_alive(0),
        "CRITICAL: pid 0 must be rejected to prevent process group false positive"
    );

    // 2. pid = 1 MUST be alive (systemd/init exists on Linux)
    assert!(
        is_pid_alive(1),
        "CRITICAL: pid 1 (init) must be reported alive"
    );

    // 3. pid = 4_194_304 (Linux architectural boundary PID_MAX_LIMIT)
    // Should be handled gracefully without overflow or panic
    let boundary_alive = is_pid_alive(PID_MAX_LIMIT);
    println!("  PID_MAX_LIMIT (4_194_304) alive: {}", boundary_alive);

    // 4. pid = 4_194_305 (PID_MAX_LIMIT + 1) MUST be rejected
    assert!(
        !is_pid_alive(PID_MAX_LIMIT + 1),
        "CRITICAL: pid > PID_MAX_LIMIT must be rejected"
    );

    // 5. pid = u32::MAX MUST be rejected
    assert!(
        !is_pid_alive(u32::MAX),
        "CRITICAL: u32::MAX must be rejected"
    );

    // 6. Negative numbers cast to u32 MUST all be rejected
    let negative_cases: &[i32] = &[-1, -2, -100, -65536, i32::MIN];
    for &neg in negative_cases {
        let as_u32 = neg as u32;
        assert!(
            !is_pid_alive(as_u32),
            "CRITICAL: negative pid {} ({}) must be rejected",
            neg,
            as_u32
        );
    }

    // 7. Test own PID
    let my_pid = std::process::id();
    assert!(
        is_pid_alive(my_pid),
        "Current test process (pid: {}) must be alive",
        my_pid
    );

    // 8. Test lifecycle of dynamically spawned child process
    let mut child = Command::new("sleep")
        .arg("5")
        .spawn()
        .expect("Failed to spawn sleep child");
    let child_pid = child.id();

    assert!(
        is_pid_alive(child_pid),
        "Dynamically spawned child {} must be alive",
        child_pid
    );

    // Kill and reap child
    let _ = child.kill();
    let _ = child.wait();

    // After reaping, child is no longer alive
    assert!(
        !is_pid_alive(child_pid),
        "Reaped child {} must no longer be alive",
        child_pid
    );

    println!("[PASS] is_pid_alive empirical boundaries verified successfully.");
}

// ==============================================================================
// 2. is_daemon_process Empirical Tests (Zombies & Argument Spoofing)
// ==============================================================================

#[test]
fn test_empirical_is_daemon_process_zombie_rejection() {
    println!("[TEST] Verifying is_daemon_process rejects zombie processes...");

    // Fork a child process that immediately exits, leaving it in zombie state until reaped
    let pid = unsafe { fork() };
    assert!(pid >= 0, "fork() failed");

    if pid == 0 {
        // Child: set comm to "kread_daemon" to attempt masquerade, then exit immediately
        let comm_name = b"kread_daemon\0";
        unsafe {
            prctl(
                PR_SET_NAME,
                comm_name.as_ptr() as usize,
                0,
                0,
                0,
            );
            _exit(0);
        }
    }

    let zombie_pid = pid as u32;

    // Give kernel a brief moment to transition child to zombie (Z) state
    thread::sleep(Duration::from_millis(50));

    // Confirm the process is indeed in zombie state
    let status_path = format!("/proc/{}/status", zombie_pid);
    let status_content = fs::read_to_string(&status_path).expect("Failed to read zombie status");
    let is_zombie = status_content.lines().any(|l| {
        l.starts_with("State:") && (l.contains('Z') || l.contains("zombie"))
    });
    assert!(
        is_zombie,
        "Test setup error: child process {} is not in zombie state",
        zombie_pid
    );

    // EMPIRICAL ASSERTION: is_daemon_process MUST reject the zombie process
    let detected_as_daemon = is_daemon_process(zombie_pid);
    println!(
        "  Zombie child (pid: {}, comm: 'kread_daemon') is_daemon_process: {}",
        zombie_pid, detected_as_daemon
    );

    // Reap the zombie process to clean up system state
    let mut exit_status = 0;
    unsafe {
        waitpid(pid, &mut exit_status, 0);
    }

    assert!(
        !detected_as_daemon,
        "CRITICAL: is_daemon_process falsely accepted a ZOMBIE process!"
    );

    println!("[PASS] Zombie process successfully rejected.");
}

#[test]
fn test_empirical_is_daemon_process_argument_spoofing() {
    println!("[TEST] Verifying is_daemon_process rejects argument spoofing...");

    // Case A: Process whose arguments contain "kread_daemon"
    let mut spoof_child = Command::new("python3")
        .arg("-c")
        .arg("import time; time.sleep(5)")
        .arg("kread_daemon")
        .spawn()
        .expect("Failed to spawn argument-spoofing process");
    let spoof_pid = spoof_child.id();

    // Allow child to spawn
    thread::sleep(Duration::from_millis(50));

    // Verify cmdline contains "kread_daemon"
    let cmdline = fs::read(format!("/proc/{}/cmdline", spoof_pid))
        .expect("Failed to read child cmdline");
    let cmdline_str = String::from_utf8_lossy(&cmdline);
    println!("  DEBUG: child cmdline read: {:?}", cmdline_str);
    assert!(
        cmdline_str.contains("kread_daemon"),
        "cmdline must contain 'kread_daemon', was: {:?}", cmdline_str
    );

    // EMPIRICAL ASSERTION: is_daemon_process MUST NOT accept this process
    let spoof_detected = is_daemon_process(spoof_pid);
    println!(
        "  Argument spoofing child (pid: {}, cmdline: {:?}) is_daemon_process: {}",
        spoof_pid, cmdline_str, spoof_detected
    );

    let _ = spoof_child.kill();
    let _ = spoof_child.wait();

    assert!(
        !spoof_detected,
        "CRITICAL: is_daemon_process falsely accepted an argument-spoofed process!"
    );

    // Case B: Process with PR_SET_NAME "kread_daemon" but wrong binary
    let pid = unsafe { fork() };
    assert!(pid >= 0, "fork() failed");

    if pid == 0 {
        let comm_name = b"kread_daemon\0";
        unsafe {
            prctl(
                PR_SET_NAME,
                comm_name.as_ptr() as usize,
                0,
                0,
                0,
            );
        }
        // Sleep for 2 seconds then exit
        thread::sleep(Duration::from_secs(2));
        unsafe { _exit(0) };
    }

    let comm_spoof_pid = pid as u32;
    thread::sleep(Duration::from_millis(50));

    // Confirm comm is "kread_daemon"
    let comm = fs::read_to_string(format!("/proc/{}/comm", comm_spoof_pid))
        .unwrap_or_default();
    assert_eq!(comm.trim(), "kread_daemon");

    // Check exe: points to the test runner binary, not "kread_daemon"
    let comm_spoof_detected = is_daemon_process(comm_spoof_pid);
    println!(
        "  Comm-spoofed child (pid: {}, comm: {:?}) is_daemon_process: {}",
        comm_spoof_pid,
        comm.trim(),
        comm_spoof_detected
    );

    unsafe {
        kill(pid, 9);
        let mut st = 0;
        waitpid(pid, &mut st, 0);
    }

    assert!(
        !comm_spoof_detected,
        "CRITICAL: is_daemon_process accepted comm spoofing without valid exe!"
    );

    println!("[PASS] Argument spoofing and comm spoofing successfully rejected.");
}

#[test]
fn test_empirical_is_daemon_process_true_positive() {
    println!("[TEST] Verifying is_daemon_process accepts genuine kread_daemon...");

    let daemon_bin = locate_kread_daemon_bin("x86_64");
    let mut child = Command::new(&daemon_bin)
        .arg("daemon")
        .arg("--interval")
        .arg("10")
        .spawn()
        .expect("Failed to spawn genuine kread_daemon");
    let daemon_pid = child.id();

    // Allow daemon to initialize and write PID
    thread::sleep(Duration::from_millis(150));

    let detected = is_daemon_process(daemon_pid);
    println!(
        "  Genuine daemon (pid: {}, binary: {:?}) is_daemon_process: {}",
        daemon_pid, daemon_bin, detected
    );

    // Send SIGTERM to terminate cleanly
    unsafe {
        kill(daemon_pid as i32, 15);
    }
    let _ = child.wait();

    assert!(
        detected,
        "CRITICAL: is_daemon_process failed to detect genuine kread_daemon!"
    );

    println!("[PASS] Genuine kread_daemon process successfully detected.");
}

// ==============================================================================
// 3. Signal Delivery Empirical Tests (Unified 152B SigAction)
// ==============================================================================

#[test]
fn test_empirical_sigaction_struct_delivery_x86_64() {
    println!("[TEST] Verifying direct SigAction syscall delivery (152B struct)...");

    assert_eq!(std::mem::size_of::<SigAction>(), 152);
    assert_eq!(std::mem::align_of::<SigAction>(), 8);

    TEST_SIGNAL_RECEIVED.store(0, Ordering::SeqCst);

    let mut sa: SigAction = unsafe { std::mem::zeroed() };
    sa.sa_handler = Some(test_handler);
    sa.sa_flags = 0;

    let ret = unsafe { sigaction(10, &sa, std::ptr::null_mut()) }; // SIGUSR1 = 10
    assert_eq!(ret, 0, "sigaction(SIGUSR1) failed");

    // Trigger SIGUSR1
    let my_pid = std::process::id() as i32;
    let kill_ret = unsafe { kill(my_pid, 10) };
    assert_eq!(kill_ret, 0, "kill(SIGUSR1) failed");

    // Allow asynchronous signal delivery
    let deadline = Instant::now() + Duration::from_millis(200);
    while TEST_SIGNAL_RECEIVED.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }

    // Verify signal was received and handled
    let received = TEST_SIGNAL_RECEIVED.load(Ordering::SeqCst);
    assert_eq!(
        received, 10,
        "Signal handler failed to record SIGUSR1 delivery"
    );

    println!("[PASS] Direct SigAction syscall delivery verified on host.");
}

#[test]
fn test_empirical_daemon_signal_termination_x86_64() {
    println!("[TEST] Verifying x86_64 kread_daemon signal delivery & clean exit (SIGINT & SIGTERM)...");

    let daemon_bin = locate_kread_daemon_bin("x86_64");

    for &sig in &[2i32, 15i32] {
        let sig_name = if sig == 2 { "SIGINT" } else { "SIGTERM" };
        println!("  Testing signal {} ({}) on x86_64 daemon...", sig, sig_name);

        let mut child = Command::new(&daemon_bin)
            .arg("daemon")
            .arg("--interval")
            .arg("10")
            .spawn()
            .expect("Failed to start daemon");
        let pid = child.id();

        // Wait for daemon to initialize self-pipe and signal handlers
        thread::sleep(Duration::from_millis(150));
        assert!(is_pid_alive(pid), "Daemon process {} died prematurely", pid);

        let t0 = Instant::now();
        let ret = unsafe { kill(pid as i32, sig) };
        assert_eq!(ret, 0, "kill({}) failed", sig);

        // Wait with a 2-second timeout
        let status = child.wait().expect("Failed to wait on daemon child");
        let elapsed = t0.elapsed();

        println!(
            "    Daemon terminated in {:?} with exit status: {:?}",
            elapsed, status
        );

        assert!(
            elapsed < Duration::from_secs(2),
            "Daemon took too long to terminate ({:?}); signal might not have interrupted poll() promptly!",
            elapsed
        );
        assert!(
            status.success(),
            "Daemon exit status was not success: {:?}",
            status
        );
    }

    println!("[PASS] x86_64 daemon cleanly handled both SIGINT and SIGTERM.");
}

#[test]
fn test_empirical_daemon_signal_termination_aarch64_qemu() {
    println!("[TEST] Verifying aarch64 musl kread_daemon signal delivery under QEMU...");

    let aarch64_bin = locate_kread_daemon_bin("aarch64");
    assert!(
        aarch64_bin.exists(),
        "aarch64 binary does not exist at {:?}",
        aarch64_bin
    );

    // Verify qemu-aarch64 is in PATH or available
    let qemu_path = match Command::new("which").arg("qemu-aarch64").output() {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => "/tmp/qemu_extract/usr/bin/qemu-aarch64".to_string(),
    };

    if !Path::new(&qemu_path).exists() {
        eprintln!("WARNING: qemu-aarch64 not found, skipping QEMU signal test");
        return;
    }

    for &sig in &[2i32, 15i32] {
        let sig_name = if sig == 2 { "SIGINT" } else { "SIGTERM" };
        println!("  Testing signal {} ({}) on aarch64 daemon under QEMU...", sig, sig_name);

        let mut child = Command::new(&qemu_path)
            .arg(&aarch64_bin)
            .arg("daemon")
            .arg("--interval")
            .arg("10")
            .spawn()
            .expect("Failed to start aarch64 daemon under QEMU");
        let pid = child.id();

        thread::sleep(Duration::from_millis(250));
        assert!(is_pid_alive(pid), "QEMU daemon process {} died prematurely", pid);

        let t0 = Instant::now();
        let ret = unsafe { kill(pid as i32, sig) };
        assert_eq!(ret, 0, "kill({}) to QEMU process failed", sig);

        let status = child.wait().expect("Failed to wait on QEMU child");
        let elapsed = t0.elapsed();

        println!(
            "    QEMU daemon terminated in {:?} with exit status: {:?}",
            elapsed, status
        );

        assert!(
            elapsed < Duration::from_secs(3),
            "QEMU daemon took too long to terminate ({:?})",
            elapsed
        );
        assert!(
            status.success(),
            "QEMU daemon exit status was not success: {:?}",
            status
        );
    }

    println!("[PASS] aarch64 daemon under QEMU cleanly handled both SIGINT and SIGTERM.");
}
