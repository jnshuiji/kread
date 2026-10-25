#!/system/bin/sh
# KernelSU / Magisk module customize.sh for kread

# Print banner
ui_print() {
    echo "$1"
}

ui_print "**********************************************"
ui_print "       kread Companion Installer             "
ui_print "  High-Concurrency GKI 6.12 Memory Engine    "
ui_print "**********************************************"

# 1. Architecture Check
ui_print "- Verifying CPU architecture..."
if [ "$ARCH" != "arm64" ] && [ "$(uname -m)" != "aarch64" ]; then
    ui_print "! Unsupported architecture: ${ARCH:-$(uname -m)}"
    ui_print "! kread requires an arm64 (aarch64) device."
    abort "! Aborting installation."
fi
ui_print "- Architecture: arm64 / aarch64 (OK)"

# 2. Kernel Version Compatibility Check
ui_print "- Checking Linux kernel version..."
KERNEL_VER=$(uname -r)
ui_print "- Detected kernel: $KERNEL_VER"
case "$KERNEL_VER" in
    *6.12*)
        ui_print "- Kernel version matches GKI 6.12 target (OK)"
        ;;
    *)
        ui_print "! Notice: Kernel is $KERNEL_VER (expected 6.12 GKI)."
        ui_print "! Module was compiled against Android GKI 6.12."
        ;;
esac

# 3. Set Permissions
ui_print "- Setting module file permissions..."
if command -v set_perm_recursive >/dev/null 2>&1; then
    set_perm_recursive "$MODPATH" 0 0 0755 0644
    set_perm "$MODPATH/service.sh" 0 0 0755
    set_perm "$MODPATH/action.sh" 0 0 0755
    set_perm "$MODPATH/bin/kread_daemon" 0 0 0755
    set_perm "$MODPATH/kread.ko" 0 0 0644
else
    chmod 0755 "$MODPATH" 2>/dev/null || true
    chmod 0755 "$MODPATH/service.sh" 2>/dev/null || true
    chmod 0755 "$MODPATH/action.sh" 2>/dev/null || true
    chmod 0755 "$MODPATH/bin" 2>/dev/null || true
    chmod 0755 "$MODPATH/bin/kread_daemon" 2>/dev/null || true
    chmod 0644 "$MODPATH/kread.ko" 2>/dev/null || true
    chmod 0644 "$MODPATH/module.prop" 2>/dev/null || true
fi

ui_print "- kread Companion installed successfully!"
