#!/system/bin/sh
# KernelSU / Magisk module customize.sh for kread

# Print banner
ui_print() {
    echo "$1"
}

ui_print "**********************************************"
ui_print "       kread Companion Installer             "
ui_print "  High-Concurrency GKI Multi-Kernel Engine    "
ui_print "**********************************************"

# 1. Architecture Check
ui_print "- Verifying CPU architecture..."
if [ "$ARCH" != "arm64" ] && [ "$(uname -m)" != "aarch64" ]; then
    ui_print "! Unsupported architecture: ${ARCH:-$(uname -m)}"
    ui_print "! kread requires an arm64 (aarch64) device."
    abort "! Aborting installation."
fi
ui_print "- Architecture: arm64 / aarch64 (OK)"

# 2. Kernel Version Compatibility Check & Auto-Selection
ui_print "- Checking Linux kernel version..."
KERNEL_VER=$(uname -r)
ui_print "- Detected kernel: $KERNEL_VER"

MATCH_FOUND=0
case "$KERNEL_VER" in
    *5.10*)
        ui_print "- Kernel matches Android 12 GKI 5.10"
        if [ -f "$MODPATH/kread_5.10.ko" ]; then
            cp -f "$MODPATH/kread_5.10.ko" "$MODPATH/kread.ko"
            MATCH_FOUND=1
        fi
        ;;
    *6.12*)
        ui_print "- Kernel matches Android 16 GKI 6.12"
        if [ -f "$MODPATH/kread_6.12.ko" ]; then
            cp -f "$MODPATH/kread_6.12.ko" "$MODPATH/kread.ko"
            MATCH_FOUND=1
        fi
        ;;
    *)
        ui_print "! Notice: Kernel is $KERNEL_VER (expected 5.10 or 6.12 GKI)."
        ;;
esac

if [ "$MATCH_FOUND" -eq 1 ]; then
    ui_print "- Successfully linked target driver to kread.ko (OK)"
elif [ -f "$MODPATH/kread.ko" ]; then
    ui_print "- Found packaged kread.ko, proceeding with standard driver"
else
    ui_print "! Error: No compatible kread.ko found for kernel $KERNEL_VER"
    abort "! Aborting installation due to missing driver."
fi

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
    chmod 0644 "$MODPATH"/*.ko 2>/dev/null || true
    chmod 0644 "$MODPATH/module.prop" 2>/dev/null || true
fi

ui_print "- kread Companion installed successfully!"
