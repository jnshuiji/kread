#!/system/bin/sh
# KernelSU late_start service script for kread

MODDIR=${0%/*}

# 1. Wait for Android boot completion
until [ "$(getprop sys.boot_completed)" = "1" ]; do
    sleep 2
done

# 2. Insert kernel module if not already loaded
if ! lsmod | grep -q "^kread "; then
    if [ -f "$MODDIR/kread.ko" ]; then
        insmod "$MODDIR/kread.ko"
    fi
fi

# 3. Ensure /dev/kread character device exists and has proper permissions
for i in 1 2 3 4 5; do
    if [ -e /dev/kread ]; then
        chmod 0666 /dev/kread
        break
    fi
    sleep 1
done

# 4. Launch background daemon if binary is available
DAEMON_BIN="$MODDIR/bin/kread_daemon"
if [ -x "$DAEMON_BIN" ]; then
    # Kill any stale instance before starting new one
    killall kread_daemon 2>/dev/null || true
    nohup "$DAEMON_BIN" daemon > /dev/null 2>&1 &
fi
