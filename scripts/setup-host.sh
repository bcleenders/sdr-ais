#!/usr/bin/env bash
#
# One-time privileged host setup. Run with sudo:
#
#     sudo ./scripts/setup-host.sh
#
# Everything else in this repo runs unprivileged.
#
# Two things need root:
#
#   1. Stopping the DVB-T kernel driver from claiming the dongle. The RTL-SDR
#      Blog V3 is electrically a TV receiver and reports USB ID 0bda:2838, so
#      Linux binds dvb_usb_rtl28xxu to it on plug-in. A USB interface can only
#      have one driver, so userspace cannot claim it until that one lets go.
#
#   2. A udev rule granting the plugdev group access to the raw USB node, so
#      the tap does not have to run as root.

set -euo pipefail

MODPROBE_CONF=/etc/modprobe.d/blacklist-rtlsdr.conf
UDEV_RULE=/etc/udev/rules.d/60-rtlsdr.rules
TARGET_USER="${SUDO_USER:-}"

if [[ $EUID -ne 0 ]]; then
    echo "error: must run as root (use sudo)" >&2
    exit 1
fi

echo "==> Blacklisting DVB-T drivers for the RTL2832U"
cat > "$MODPROBE_CONF" <<'EOF'
# The RTL-SDR is a repurposed DVB-T receiver. These drivers claim the USB
# interface on plug-in and prevent SDR use. Blacklisted by sdr-ais setup.
blacklist dvb_usb_rtl28xxu
blacklist rtl2832_sdr
blacklist rtl2832
blacklist dvb_usb_v2
EOF
echo "    wrote $MODPROBE_CONF"

echo "==> Unloading them now (so you do not have to reboot)"
# Order matters: dependants first. Failures are fine if already unloaded, or
# if something else on the system holds a reference.
for mod in rtl2832_sdr dvb_usb_rtl28xxu rtl2832 dvb_usb_v2; do
    if lsmod | grep -q "^${mod} "; then
        if modprobe -r "$mod" 2>/dev/null; then
            echo "    unloaded $mod"
        else
            echo "    could not unload $mod (in use) — replug the dongle or reboot"
        fi
    fi
done

echo "==> Installing udev rule for unprivileged USB access"
cat > "$UDEV_RULE" <<'EOF'
# RTL-SDR Blog V3 (Realtek RTL2832U). Grants the plugdev group read/write on
# the raw USB node so sdr-ais-host can run without root.
SUBSYSTEM=="usb", ATTRS{idVendor}=="0bda", ATTRS{idProduct}=="2838", MODE="0660", GROUP="plugdev", TAG+="uaccess"
EOF
echo "    wrote $UDEV_RULE"

udevadm control --reload-rules
udevadm trigger --subsystem-match=usb --attr-match=idVendor=0bda
echo "    reloaded udev"

echo "==> Checking group membership"
if [[ -n "$TARGET_USER" ]]; then
    for group in plugdev kvm; do
        if id -nG "$TARGET_USER" | tr ' ' '\n' | grep -qx "$group"; then
            echo "    $TARGET_USER is in $group"
        else
            echo "    adding $TARGET_USER to $group"
            usermod -aG "$group" "$TARGET_USER"
            echo "    NOTE: log out and back in for this to take effect"
        fi
    done
else
    echo "    skipped (could not determine invoking user)"
fi

echo "==> Keeping the user session alive across logouts"
# vm-up.sh puts its sockets in /run/user/$UID, which systemd deletes when the
# last session for that user ends. Lingering keeps the VM running after you
# close an SSH connection.
if [[ -n "$TARGET_USER" ]]; then
    loginctl enable-linger "$TARGET_USER" && echo "    linger enabled for $TARGET_USER"
fi

echo
echo "Done. Verify with:"
echo "    lsusb | grep 0bda:2838        # dongle present"
echo "    lsmod | grep dvb_usb_rtl28xxu # should print nothing"
echo
echo "If the dongle was plugged in before this ran, replug it now."
