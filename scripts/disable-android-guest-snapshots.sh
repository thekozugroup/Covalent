#!/bin/sh
set -eu

# Turn off WindowManager's task snapshots on an API 37 guest, before anything
# judges that guest.
#
# WHY THIS EXISTS
#
# On this image's linux-x86_64 gfxstream stack, any CPU lock of a host-backed
# graphics buffer aborts the process that does it:
#
#   Assertion failed: !rcEnc->featureInfo()->hasReadColorBufferDma
#     GoldfishMapper::readFromHost  /  Gralloc5Mapper::lock
#
# scripts/disable-android-guest-launcher.sh removed one registrant of that fault
# - NexusLauncher's CompositionSamplingListener, CPU-locked by SurfaceFlinger's
# RegionSamplingThread. This removes the other one the instrumentation suite
# reaches. WindowManager records a recents thumbnail whenever an activity
# finishes - once per test, 56 times - and the persist step CPU-locks the
# captured buffer:
#
#   name: TaskSnapshotPer >>> system_server <<<
#     at TaskSnapshotConvertUtil.copyToSwBitmapDirect
#     at SnapshotPersistQueue$StoreWriteQueueItem.writeBuffer
#
# WHY THIS LEVER, AND WHY NOT A PROPERTY
#
# AbsAppSnapshotController.shouldDisableSnapshots() reads three instance
# booleans - mIsRunningOnTv, mIsRunningOnIoT, mSnapshotEnabled - and no
# SystemProperties and no DeviceConfig at all. Confirmed by dexdumping this
# image's own services.jar. So no `setprop`, no `device_config` and no
# `cmd window` subcommand can reach it, and the earlier negative results on that
# ground were complete rather than badly aimed. The one reachable switch is
# IWindowManager.setTaskSnapshotEnabled(boolean), which WindowManagerService
# implements as a bare `mTaskSnapshotController.setSnapshotEnabled(enabled)`
# with no permission check, so `service call window` from the shell uid reaches
# it. It gates the capture upstream of the persist queue that aborts.
#
# WHY THE TRANSACTION CODE IS DERIVED AND NEVER HARDCODED
#
# `service call` addresses a binder method by its transaction code, which is
# just the method's ordinal in IWindowManager.aidl. It moves whenever a method
# is added to or removed from that interface, so it is a property of the system
# image, not of this repository - a constant read from one image and used
# against another silently calls a *different method* and reports success. The
# value happens to be 137 on the arm64 image it was first read from; CI runs
# x86_64, so this reads it out of the booted guest's own framework.jar instead.
#
# Two details here are load-bearing and must not be tidied away:
#   * `dexdump` runs WITHOUT `-d`. A static final int's value is in the field
#     listing, which comes back in a fraction of a second; disassembling the
#     same dex emits ~2.07 million lines and takes minutes. That is the
#     difference between affordable during CI setup and not.
#   * `LC_ALL=C` on the grep and the awk. The dump carries the dex string pool
#     verbatim, which holds byte sequences that are not valid text in a UTF-8
#     locale; awk aborts mid-stream on them and the derivation silently comes
#     back empty. This reproduces in CI and not on every host, so it is not
#     cosmetic.
#
# SCOPE LIMIT - READ THIS BEFORE TRUSTING IT FURTHER
#
# This fixes the *suite*, not the platform. The gfxstream defect is untouched.
# Any future test that captures an image - captureToImage, takeScreenshot,
# PixelCopy, any Bitmap readback - will CPU-lock a host-backed buffer and
# re-trigger the identical assertion, and so will any other framework component
# that does. Today's 56 tests contain no such call, which is the only reason
# disabling this one producer is sufficient. It also disables only the *task*
# snapshot controller: ActivitySnapshotController keeps its own separate
# mSnapshotEnabled and IWindowManager exposes no setter for it.

serial=${1:-}
if [ -z "$serial" ]; then
  echo "usage: disable-android-guest-snapshots.sh <serial>" >&2
  exit 1
fi

android_sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
if [ -z "$android_sdk" ]; then
  echo "Android SDK not found; set ANDROID_HOME or ANDROID_SDK_ROOT." >&2
  exit 1
fi
adb="$android_sdk/platform-tools/adb"
if [ ! -x "$adb" ]; then
  echo "adb is missing from $android_sdk/platform-tools." >&2
  exit 1
fi

dexdump=""
for candidate in "$android_sdk"/build-tools/*/dexdump; do
  [ -x "$candidate" ] && dexdump=$candidate
done
if [ -z "$dexdump" ]; then
  echo "dexdump not found under $android_sdk/build-tools." >&2
  exit 1
fi

# Unpack under the system temp dir, never the checkout: framework.jar expands to
# tens of megabytes of dex and would otherwise land in the working tree.
tmp_root=${TMPDIR:-/tmp}
case "$tmp_root" in
  /*) ;;
  *) tmp_root=/tmp ;;
esac
work=$(mktemp -d "$tmp_root/covalent-wmprobe.XXXXXX")
cleanup() { rm -rf "$work"; }
trap cleanup EXIT INT TERM

echo "API-37-snapshots: reading the setTaskSnapshotEnabled transaction code off $serial"

if ! "$adb" -s "$serial" pull /system/framework/framework.jar "$work/framework.jar" >/dev/null 2>&1; then
  echo "could not pull /system/framework/framework.jar from $serial" >&2
  exit 1
fi
if ! (cd "$work" && unzip -o -q framework.jar -d dex) 2>/dev/null; then
  echo "could not unpack framework.jar" >&2
  exit 1
fi

# grep the containers first so dexdump runs at most once: the constant's name
# appears verbatim in the string pool of whichever dex holds IWindowManager$Stub.
transaction=""
for dex in "$work"/dex/classes*.dex; do
  [ -f "$dex" ] || continue
  LC_ALL=C grep -qa TRANSACTION_setTaskSnapshotEnabled "$dex" || continue
  transaction=$("$dexdump" "$dex" 2>/dev/null | LC_ALL=C awk '
    /\(in Landroid\/view\/IWindowManager\$Stub;\)/ { inclass = 1; found = 0; next }
    /\(in L/ { inclass = 0; found = 0 }
    inclass && /name +: .TRANSACTION_setTaskSnapshotEnabled./ { found = 1; next }
    found && /value +:/ { gsub(/[^0-9]/, "", $0); print; exit }
  ')
  [ -n "$transaction" ] && break
done

case "$transaction" in
  '' )
    echo "could not derive TRANSACTION_setTaskSnapshotEnabled from $serial's framework.jar." >&2
    echo "IWindowManager may no longer declare setTaskSnapshotEnabled on this image; read it before guessing." >&2
    exit 1
    ;;
  *[!0-9]* )
    echo "derived a non-numeric transaction code '$transaction'; refusing to call it." >&2
    exit 1
    ;;
esac
echo "API-37-snapshots: transaction code $transaction"

# Read the value that belongs to the *task* controller specifically.
#
# `dumpsys window` prints one mSnapshotEnabled per snapshot controller - this
# image has two, TaskSnapshotController and ActivitySnapshotController - and it
# prints each controller's name on the line *after* its fields. So the trailing
# "SnapshotCache Task" label is the only thing that tells the two values apart.
# Matching any mSnapshotEnabled=false anywhere in the dump would pass while the
# task controller was still enabled and the activity one merely happened to be
# off, which is the one way this verification could report success over an
# unfixed guest. Empty output means "could not be read", never "false".
task_snapshot_enabled() {
  "$adb" -s "$serial" shell dumpsys window 2>/dev/null |
    tr -d '\r' |
    awk '/^[[:space:]]*mSnapshotEnabled=/ {
           sub(/^[[:space:]]*mSnapshotEnabled=/, "")
           pending = $0
           next
         }
         /^[[:space:]]*SnapshotCache[[:space:]]+Task[[:space:]]*$/ && pending != "" {
           print pending
           exit
         }'
}

all_snapshot_states() {
  "$adb" -s "$serial" shell dumpsys window 2>/dev/null |
    tr -d '\r' |
    grep -o 'mSnapshotEnabled=[a-z]*' |
    tr '\n' ' '
}

before=$(task_snapshot_enabled)
if [ -z "$before" ]; then
  echo "dumpsys window did not report a 'SnapshotCache Task' block on $serial." >&2
  echo "the switch cannot be verified, so it is not being called." >&2
  exit 1
fi
echo "API-37-snapshots: before: task mSnapshotEnabled=$before (all controllers: $(all_snapshot_states))"

# The call's own exit status is not evidence - `service call` reports success for
# a transaction the service ignored - so it is deliberately not trusted here. The
# read-back below is what decides.
"$adb" -s "$serial" shell service call window "$transaction" i32 0 >/dev/null 2>&1 || true

after=$(task_snapshot_enabled)
echo "API-37-snapshots: after:  task mSnapshotEnabled=${after:-unreadable} (all controllers: $(all_snapshot_states))"
if [ "$after" != "false" ]; then
  echo "the call did not disable task snapshots on $serial (task mSnapshotEnabled=${after:-unreadable})." >&2
  echo "transaction $transaction was derived from this guest's own framework.jar, so this means the" >&2
  echo "method moved or stopped taking effect - read IWindowManager again rather than trying another number." >&2
  exit 1
fi

echo "API-37-snapshots: framework at handover: $("$adb" -s "$serial" shell ps -A -o PID,ETIME,NAME 2>/dev/null |
  tr -d '\r' |
  awk '$3 == "surfaceflinger" { sf = $1 " up " $2 }
       $3 == "system_server" { ss = $1 " up " $2 }
       END { printf "surfaceflinger %s, system_server %s", (sf ? sf : "absent"), (ss ? ss : "absent") }')"
