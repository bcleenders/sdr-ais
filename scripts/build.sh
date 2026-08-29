#!/usr/bin/env bash
#
# Build everything the deployment needs:
#   1. the host binaries (native)
#   2. the guest binary (static, musl — it must run with no libc in the rootfs)
#   3. a minimal ext4 root filesystem containing only that binary
#
# No privileges required. The rootfs is populated with `mke2fs -d`, which fills
# a filesystem image from a directory without mounting anything.

source "$(dirname "${BASH_SOURCE[0]}")/config.sh"

cd "$REPO_ROOT"

# --- Toolchain checks --------------------------------------------------------
command -v cargo >/dev/null || die "cargo not found (try: source \$HOME/.cargo/env)"
command -v mke2fs >/dev/null || die "mke2fs not found (install e2fsprogs)"

if ! rustup target list --installed 2>/dev/null | grep -qx "$GUEST_TARGET"; then
    log "Adding Rust target $GUEST_TARGET"
    rustup target add "$GUEST_TARGET"
fi

# --- Host binaries -----------------------------------------------------------
log "Building host binaries"
cargo build --release -p sdr-ais-host

# --- Guest binary ------------------------------------------------------------
# Statically linked on purpose: the rootfs has no /lib, no dynamic loader and
# no shell, so there is nothing for a compromised process to fall back to.
log "Building guest binary ($GUEST_TARGET, static)"
cargo build --release --target "$GUEST_TARGET" -p sdr-ais-guest

[[ -f "$GUEST_BIN" ]] || die "guest binary missing at $GUEST_BIN"

if command -v ldd >/dev/null && ldd "$GUEST_BIN" 2>&1 | grep -q "=>"; then
    die "guest binary is dynamically linked; it will not boot in the minimal rootfs"
fi

# --- Kernel ------------------------------------------------------------------
mkdir -p "$VM_DIR"

if [[ ! -f "$KERNEL" ]]; then
    # Reuse a kernel from a previous Firecracker experiment if one is around.
    FALLBACK="$HOME/src/firecracker-getting-started/vmlinux-6.1.174"
    if [[ -f "$FALLBACK" ]]; then
        log "Copying kernel from $FALLBACK"
        cp "$FALLBACK" "$KERNEL"
    else
        die "no kernel at $KERNEL

Fetch an uncompressed Firecracker guest kernel, for example:
  curl -fSLo '$KERNEL' \\
    https://s3.amazonaws.com/spec.ccfc.min/firecracker-ci/v1.10/x86_64/vmlinux-6.1.102

It must have CONFIG_VIRTIO_VSOCKETS and CONFIG_EXT4_FS built in."
    fi
fi

# --- Root filesystem ---------------------------------------------------------
log "Building root filesystem"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

# The entire guest userland. /init is PID 1; /proc is the only mount it makes.
install -Dm0755 "$GUEST_BIN" "$STAGE/init"
mkdir -p "$STAGE/proc" "$STAGE/dev"

rm -f "$ROOTFS"
# 64 MiB is far more than the ~10 MiB binary needs, and the image is mounted
# read-only anyway, so there is nothing to grow into.
mke2fs -q -F -t ext4 -b 4096 -d "$STAGE" -L sdr-ais "$ROOTFS" 64M

log "Done"
printf '    host binary : %s\n' "$HOST_BIN"
printf '    guest binary: %s (%s)\n' "$GUEST_BIN" "$(du -h "$GUEST_BIN" | cut -f1)"
printf '    kernel      : %s\n' "$KERNEL"
printf '    rootfs      : %s (%s)\n' "$ROOTFS" "$(du -h "$ROOTFS" | cut -f1)"
printf '\n    contents of the guest filesystem:\n'
printf '      /init  (the only executable)\n      /proc  /dev\n'
