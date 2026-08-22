#!/bin/sh
# Take the home app off an emulator guest before anything waits on that guest.
#
# WHY THIS EXISTS
#
# On the CI runner's linux-x86_64 API 37 google_apis x86_64 image, surfaceflinger
# aborts roughly every 29 seconds for the whole life of the guest:
#
#   Executable: /system/bin/surfaceflinger   Process uptime: 30s
#   name: RegionSampling  >>> /system/bin/surfaceflinger <<<
#   signal 6 (SIGABRT), code -1 (SI_QUEUE)
#   Abort message: 'Assertion failed: !rcEnc->featureInfo()->hasReadColorBufferDma'
#     #03 mapper.ranchu.so  GoldfishMapper::readFromHost(cb_handle_t const&)
#     #05 libui.so          Gralloc5Mapper::lock(...)
#     #09 surfaceflinger    RegionSamplingThread::threadMain()
#
# system_server follows surfaceflinger down, the framework restarts, and the
# registrant comes back and registers again. That single loop is every symptom
# the API 37 device gate has ever reported: a runqueue that never drains because
# each cycle is a fresh boot storm, binder round trips timing out at random, and
# ComponentActivity failing to resolve even at a low runqueue - user 0 is briefly
# re-locked after each restart and Direct Boot then matches only
# direct-boot-aware components.
#
# The registrant is named by the guest itself. Run 32543425657's event buffer
# carries, interleaved with the surfaceflinger aborts:
#
#   am_crash: [...,com.google.android.apps.nexuslauncher,...,
#              java.lang.RuntimeException,Couldn't removeRegionSamplingListener,
#              CompositionSamplingListener.java,-2,0]
#
# The launcher registers a CompositionSamplingListener to choose status bar icon
# contrast, SurfaceFlinger's RegionSamplingThread CPU-locks the composited buffer
# to read it, and the guest's Gralloc5 mapper asserts that the host has not
# advertised ANDROID_EMU_read_color_buffer_dma. Remove the registrant and nothing
# asks SurfaceFlinger to sample, so the thread never locks a buffer and the
# assertion is never reached.
#
# WHY THIS IS A FIX AND NOT A WORKAROUND
#
# Nothing this suite tests involves a launcher. Every instrumentation test drives
# the app's own activities through ActivityScenario / InstrumentationActivity-
# Invoker, which start components by explicit ComponentName; none of them goes
# through the home intent, and none of them reads the home screen. The launcher
# on this guest is not a dependency, it is the one process making the device
# unusable. Taking it off is acting at the proven cause.
#
# WHY IT RUNS BEFORE wait-for-android-guest.sh RATHER THAN INSIDE IT
#
# The readiness script's whole job is to decide whether a guest is fit to be
# tested, and while this loop is running no guest ever is - the readiness
# conditions are exactly the ones the loop breaks. Repairing the guest from
# inside the script that judges it would mean the judgement could never be made
# on an unrepaired guest, and would hide how long the repair took. It is a
# separate step, run first, that reports what it did.
#
# WHAT IT DOES NOT CLAIM
#
# SystemUI's own RegionSamplingHelper drives nav bar contrast sampling
# independently of the launcher. The guest's crash records name only the
# launcher, but they cannot prove it is the only registrant. So this script
# proves what it can prove - that the disable was accepted and that no enabled
# home package is left - and the assertion of what that bought is left to
# scripts/wait-for-android-guest.sh, which requires surfaceflinger's and
# system_server's pids to hold across its stability window. If the abort survives
# this, the guest will say so there, and the surviving registrant is SystemUI.
set -eu

serial=${1:-}
if [ -z "$serial" ]; then
  echo "usage: disable-android-guest-launcher.sh <serial>" >&2
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

# Bounded in wall clock, like every other wait in this gate, so a guest that is
# merely slow to bring PackageManager up gets the whole budget and a wedged one
# still ends the job with a sentence rather than the workflow's timeout.
budget=${COVALENT_ANDROID_LAUNCHER_DISABLE_TIMEOUT:-420}
interval=${COVALENT_ANDROID_LAUNCHER_DISABLE_INTERVAL:-5}
probe_deadline=${COVALENT_ANDROID_GUEST_PROBE_TIMEOUT:-10}

started=$(date +%s)
elapsed() { echo $(($(date +%s) - started)); }
out_of_budget() { [ "$(elapsed)" -ge "$budget" ]; }

if command -v timeout >/dev/null 2>&1; then
  host_deadline() { timeout "$@"; }
elif command -v gtimeout >/dev/null 2>&1; then
  host_deadline() { gtimeout "$@"; }
else
  host_deadline() { shift; "$@"; }
fi

# Deadlines on both sides of adb: the guest-side one kills a binder transaction
# blocked on a system_server lock, which is the exact state this guest spends
# most of its time in, and the host-side one covers an `adb shell` that never
# establishes at all. Neither can make this step the reason the job hangs.
bounded_shell() {
  host_deadline "$((probe_deadline + 5))" \
    "$adb" -s "$serial" shell timeout "$probe_deadline" "$@" 2>/dev/null |
    tr -d '\r'
}

# The package currently holding the home role, asked of the image instead of
# assumed. `resolve-activity --brief` prints a `priority=...` line and then the
# component, and prints `No activity found` when nothing resolves, so only a line
# shaped `package/class` is read.
resolved_home_package() {
  bounded_shell cmd package resolve-activity --brief --user 0 \
    -a android.intent.action.MAIN -c android.intent.category.HOME |
    sed -n 's|^\([A-Za-z0-9_][A-Za-z0-9_.]*\)/.*$|\1|p' |
    head -n 1
}

# Named as well as resolved. The resolved answer is authoritative for the image
# that is actually booted, but it is read from a guest that is restarting every
# 29 seconds, so a run where the read lands in a dead window still has to know
# what to disable. NexusLauncher is what google_apis API 37 ships and what the
# crash records name; Launcher3 is AOSP's, kept so this script is not silently
# wrong on an image without Google apps.
launcher_candidates="${COVALENT_ANDROID_LAUNCHER_PACKAGES:-com.google.android.apps.nexuslauncher com.android.launcher3}"

# Packages the resolved answer may name that must never be disabled.
#
# This is not defensive padding, it is required for the script to be safe to run
# twice. Once the launcher is gone the home intent resolves to
# `com.android.settings/.FallbackHome` - measured, on this exact image - which is
# the platform's own no-launcher home: a bare activity that draws nothing and
# samples nothing. A second invocation would otherwise read that as "the home
# package" and disable Settings, which would take far more with it than this
# script is trying to remove. SystemUI is listed for the same reason and one
# more: its RegionSamplingHelper is the plausible second registrant, and if it
# ever needs silencing that is a decision with its own evidence behind it, not
# something this loop should arrive at by following a resolution.
launcher_never_disable="${COVALENT_ANDROID_LAUNCHER_NEVER_DISABLE:-com.android.settings com.android.systemui android}"

echo "API-37-launcher: disabling the home app on $serial (budget ${budget}s)"

# The disable is issued once per candidate and then *verified*, and the loop
# exists only for the verification. `pm disable-user` writes package settings
# that persist across the framework restarts this guest is having, so it does not
# need re-issuing to converge - but it does need to have been accepted at all,
# and a call made while system_server is down is accepted by nothing. Looping
# until `pm list packages -d` names the package is the difference between having
# disabled the launcher and having typed the command.
disabled=""
installed=""
while : ; do
  if out_of_budget; then
    echo "API-37-launcher: giving up after $(elapsed)s: PackageManager never confirmed a disabled home app." >&2
    echo "last package list read: ${installed:-(empty - PackageManagerService never answered)}" >&2
    echo "candidates: $launcher_candidates" >&2
    echo "--- crash and event log tail ---" >&2
    "$adb" -s "$serial" logcat -d -b crash -b events -t 200 >&2 2>&1 || true
    exit 1
  fi

  installed=$(bounded_shell pm list packages --user 0)
  if [ -z "$installed" ]; then
    sleep "$interval"
    continue
  fi

  pending=""
  present=""
  while IFS= read -r candidate; do
    [ -n "$candidate" ] || continue
    case " $launcher_never_disable " in
      *" $candidate "*)
        echo "API-37-launcher: home resolves to $candidate, which is never disabled; skipping it"
        continue
        ;;
    esac
    printf '%s\n' "$installed" | grep -qx "package:$candidate" || continue
    present="$present $candidate"
    case " $disabled " in
      *" $candidate "*) continue ;;
    esac
    bounded_shell pm disable-user --user 0 "$candidate" >/dev/null 2>&1 || true
    if bounded_shell pm list packages -d --user 0 | grep -qx "package:$candidate"; then
      disabled="$disabled $candidate"
      echo "API-37-launcher: $candidate disabled for user 0 after $(elapsed)s"
    else
      pending="$pending $candidate"
    fi
  done <<CANDIDATES
$(resolved_home_package)
$(printf '%s' "$launcher_candidates" | tr ' ' '\n')
CANDIDATES

  if [ -z "$present" ]; then
    # The guest answered a package list and none of the candidates was in it.
    # That is not this script failing; it is an image with no launcher, which is
    # the state this script exists to produce.
    echo "API-37-launcher: no disablable home package is installed on $serial; nothing to disable"
    break
  fi
  if [ -n "$disabled" ] && [ -z "$pending" ]; then
    break
  fi
  sleep "$interval"
done

# Proof, printed, in the guest's own words. `pm list packages -d` is the
# authoritative disabled list, and what the home intent resolves to afterwards is
# the direct statement of whether anything is still going to draw a home screen
# and sample it.
echo "API-37-launcher: disabled packages now: $(bounded_shell pm list packages -d --user 0 | tr '\n' ' ')"
echo "API-37-launcher: home intent now resolves to: $(bounded_shell cmd package resolve-activity --brief --user 0 -a android.intent.action.MAIN -c android.intent.category.HOME | tail -n 1)"

# One framework restart is expected after this point and is not a failure: the
# listener registered by the launcher that was running when the disable landed is
# still live, so the next sampling pass still aborts. The disable persists across
# that restart and the launcher does not come back. Reported rather than waited
# on - scripts/wait-for-android-guest.sh runs next and is what decides whether
# the guest converged, on evidence rather than on this script's optimism.
echo "API-37-launcher: framework at handover: $(bounded_shell ps -A -o PID,ETIME,NAME |
  awk '$3 == "surfaceflinger" { sf = $1 " up " $2 }
       $3 == "system_server" { ss = $1 " up " $2 }
       END { printf "surfaceflinger %s, system_server %s", (sf ? sf : "absent"), (ss ? ss : "absent") }')"
