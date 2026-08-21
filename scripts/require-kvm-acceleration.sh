#!/bin/sh
# Prove /dev/kvm is really usable by this user before an Android emulator job
# spends its whole timeout falling back to TCG.
#
# The predecessor of this script was three bare `test -c/-r/-w /dev/kvm` lines
# inline in a workflow. When it failed it exited 1 in about fifty milliseconds
# having printed nothing at all, so "this runner has no /dev/kvm", "the udev
# rule has not landed yet" and "the ioctl was refused" were indistinguishable
# from each other and from the step never running. Every failure path below
# therefore states what was checked, what was found, and what that means. No
# assertion is softened to get there: each one still exits non-zero.
#
# GitHub's ubuntu-24.04 image ships /dev/kvm as root:kvm mode 0660 with an empty
# `kvm` group, so the emulator's ProbeKVM reports "This user doesn't have
# permissions to use KVM (/dev/kvm)" and silently drops to software emulation.
# Installing the udev rule the emulator-runner action documents fixes that; the
# live KVM_GET_API_VERSION ioctl at the end is what proves it actually worked.
set -eu

fail() {
  echo "KVM acceleration gate failed: $1" >&2
  shift
  for line in "$@"; do
    echo "  $line" >&2
  done
  echo "  This job requires hardware acceleration; software emulation cannot" >&2
  echo "  boot an API 37 google_apis x86_64 guest inside the step timeout." >&2
  exit 1
}

if [ "$(uname -s)" != Linux ]; then
  fail "this gate only runs on Linux emulator runners" \
    "uname -s reported $(uname -s), which has no /dev/kvm."
fi

# Evidence first. These lines run before any assertion, so the log explains a
# failure even when the very first check is the one that fails.
echo "runner user: $(id)"
echo "kernel: $(uname -srm)"
if [ -e /dev/kvm ]; then
  echo "/dev/kvm: $(ls -l /dev/kvm)"
else
  echo "/dev/kvm: absent before the udev rule was installed"
fi
echo "kvm kernel modules: $(lsmod 2>/dev/null | awk '$1 ~ /^kvm/ { printf "%s ", $1 }' || true)"
echo "cpu virtualisation flags: $(awk '/^flags/ { for (i = 1; i <= NF; i++) if ($i == "vmx" || $i == "svm") { print $i; exit } }' /proc/cpuinfo || true)"

if ! command -v sudo >/dev/null 2>&1; then
  fail "sudo is unavailable, so the /dev/kvm access rule cannot be installed" \
    "Grant this job sudo, or pre-provision /dev/kvm as world read-write."
fi

rule='KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"'
if ! printf '%s\n' "$rule" | sudo tee /etc/udev/rules.d/99-kvm4all.rules >/dev/null; then
  fail "could not write /etc/udev/rules.d/99-kvm4all.rules" \
    "The runner's root filesystem rejected the udev rule that grants KVM access."
fi
if ! sudo udevadm control --reload-rules; then
  fail "udevadm could not reload its rules" \
    "The rule was written but udev never picked it up."
fi
if ! sudo udevadm trigger --name-match=kvm; then
  fail "udevadm could not trigger a kvm uevent" \
    "The rule was loaded but was never applied to the existing device node."
fi
# `udevadm trigger` only queues the uevent; it does not wait for the new mode to
# be applied. Without `settle` the assertions below race the rule they depend on
# and can fail on a device that is about to become usable a millisecond later.
if ! sudo udevadm settle; then
  fail "udevadm settle did not complete" \
    "The kvm uevent is still queued, so /dev/kvm may not carry the new mode yet."
fi

if [ ! -e /dev/kvm ]; then
  fail "/dev/kvm does not exist on this runner" \
    "The kernel exposes no KVM device node at all, so this is not a" \
    "permissions problem: the runner is not virtualisation-capable, or the" \
    "kvm_intel/kvm_amd module is not loaded. Check the module and CPU flag" \
    "lines above."
fi
if [ ! -c /dev/kvm ]; then
  fail "/dev/kvm exists but is not a character device" \
    "Found: $(ls -l /dev/kvm)" \
    "Something has replaced the device node with a regular file or directory."
fi
echo "/dev/kvm after the udev rule: $(ls -l /dev/kvm)"
if [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then
  fail "/dev/kvm is not readable and writable by this user" \
    "Found: $(ls -l /dev/kvm)" \
    "Current user: $(id)" \
    "The udev rule landed but the mode is still restrictive, so the emulator" \
    "would report \"This user doesn't have permissions to use KVM\" and fall" \
    "back to TCG instead of failing."
fi

# Permission bits are not proof. Open the device and ask the kernel directly, so
# a runner whose KVM subsystem is present but unusable fails here - loudly and
# in seconds - rather than timing out ten minutes into a guest boot.
if ! python3 - <<'PY'
import fcntl
import os
import sys

KVM_GET_API_VERSION = 0xAE00
try:
    descriptor = os.open("/dev/kvm", os.O_RDWR | os.O_CLOEXEC)
except OSError as error:
    sys.exit(f"opening /dev/kvm for read-write failed: {error}")
try:
    version = fcntl.ioctl(descriptor, KVM_GET_API_VERSION, 0)
except OSError as error:
    sys.exit(f"KVM_GET_API_VERSION ioctl was refused: {error}")
finally:
    os.close(descriptor)
if version != 12:
    sys.exit(f"KVM reported API version {version}, not the stable 12.")
print("KVM API version 12 is usable by the current user.")
PY
then
  fail "the live KVM_GET_API_VERSION probe did not succeed" \
    "See the Python diagnosis immediately above for the exact reason." \
    "/dev/kvm was accessible by permission bits but the kernel refused to" \
    "hand out a KVM handle."
fi

echo "KVM acceleration gate passed."
