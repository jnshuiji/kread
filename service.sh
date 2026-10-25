#!/system/bin/sh
# ==============================================================================
# service.sh - Hardened KernelSU late_start service supervisor for kread
# ==============================================================================

MODDIR=${0%/*}
LOGFILE="$MODDIR/service.log"
PIDFILE="$MODDIR/kread_daemon.pid"
DAEMON_BIN="$MODDIR/bin/kread_daemon"

log() {
    echo "[$(date -u '+%Y-%m-%dT%H:%M:%SZ')] $1" >> "$LOGFILE"
}

# 1. Wait for Android boot completion with timeout (60 iterations * 2s = 120s)
BOOT_TIMEOUT=60
while [ "$(getprop sys.boot_completed)" != "1" ] && [ "$BOOT_TIMEOUT" -gt 0 ]; do
    sleep 2
    BOOT_TIMEOUT=$((BOOT_TIMEOUT - 1))
done

if [ "$(getprop sys.boot_completed)" != "1" ]; then
    log "ERROR: Boot completion timed out after 120s. Aborting service launch."
    exit 1
fi
log "INFO: Boot completed detected."

# 2. Insert kernel module with verification
if ! lsmod | grep -q "^kread "; then
    if [ -f "$MODDIR/kread.ko" ]; then
        log "INFO: Inserting $MODDIR/kread.ko..."
        insmod "$MODDIR/kread.ko" 2>> "$LOGFILE"
        if ! lsmod | grep -q "^kread "; then
            log "ERROR: insmod failed. Check dmesg for kernel symbols or vermagic mismatch."
            exit 1
        fi
        log "INFO: kread.ko loaded successfully."
    else
        log "ERROR: Module kread.ko not found in $MODDIR."
        exit 1
    fi
else
    log "INFO: kread.ko already loaded."
fi

# 3. Ensure /dev/kread character device exists with hardened permissions (0600 root:root)
DEV_FOUND=0
for i in 1 2 3 4 5; do
    if [ -e /dev/kread ]; then
        DEV_FOUND=1
        chown root:root /dev/kread 2>/dev/null || true
        chmod 0600 /dev/kread
        log "INFO: /dev/kread device node secured (mode 0600, root:root)."
        break
    fi
    sleep 1
done

if [ "$DEV_FOUND" -eq 0 ]; then
    log "ERROR: /dev/kread character device not created by kernel."
    exit 1
fi

# 4. Gracefully terminate any previous daemon instance
if [ -f "$PIDFILE" ]; then
    OLD_PID=$(cat "$PIDFILE" 2>/dev/null | tr -d '[:space:]')
    if [ -n "$OLD_PID" ] && kill -0 "$OLD_PID" 2>/dev/null; then
        log "INFO: Gracefully stopping old daemon instance (PID $OLD_PID)..."
        kill -15 "$OLD_PID" 2>/dev/null || true
        # Wait up to 5 seconds for graceful exit
        WAIT_N=10
        while kill -0 "$OLD_PID" 2>/dev/null && [ "$WAIT_N" -gt 0 ]; do
            sleep 0.5
            WAIT_N=$((WAIT_N - 1))
        done
        if kill -0 "$OLD_PID" 2>/dev/null; then
            log "WARN: Daemon did not exit within timeout, sending SIGKILL..."
            kill -9 "$OLD_PID" 2>/dev/null || true
            sleep 0.5
        fi
    fi
    rm -f "$PIDFILE"
fi

# Catch any orphaned/stale daemon processes
STALE_PIDS=$(pidof kread_daemon 2>/dev/null || true)
if [ -n "$STALE_PIDS" ]; then
    log "WARN: Cleaning up orphaned kread_daemon PIDs: $STALE_PIDS"
    for spid in $STALE_PIDS; do
        kill -15 "$spid" 2>/dev/null || true
    done
    sleep 0.5
    for spid in $STALE_PIDS; do
        kill -9 "$spid" 2>/dev/null || true
    done
fi

# 5. Launch daemon with startup verification and diagnostic status check
if [ -x "$DAEMON_BIN" ]; then
    log "INFO: Spawning background daemon..."
    export KREAD_RUNTIME_DIR="$MODDIR"
    "$DAEMON_BIN" daemon >> "$LOGFILE" 2>&1 &
    SPAWNED_PID=$!

    # Verify daemon started and remains alive
    sleep 1
    if kill -0 "$SPAWNED_PID" 2>/dev/null; then
        log "INFO: kread_daemon launched successfully (PID: $SPAWNED_PID)."
        # Run status diagnostic check
        STATUS_JSON=$("$DAEMON_BIN" status 2>/dev/null || true)
        log "STATUS: $STATUS_JSON"
    else
        log "ERROR: Daemon died immediately after launch. Inspect $LOGFILE."
        exit 1
    fi
else
    log "WARN: Daemon binary $DAEMON_BIN not found or not executable."
fi

exit 0
