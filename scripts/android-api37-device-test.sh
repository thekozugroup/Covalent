#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_root/scripts/android-instrumentation-result.sh"
android_sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
avd_name=Covalent_API_37
required_serial=emulator-5570
serial=${ANDROID_SERIAL:-}
headless_ci=${COVALENT_ANDROID_HEADLESS_CI:-false}
# Opt-in, and only ever opt-in: unset means this script builds everything it
# needs, which is what a developer running it by hand gets. See the block above
# the build below for what setting it does and does not skip.
prebuilt=${COVALENT_ANDROID_PREBUILT:-false}
tls_image=covalent:android-api37-e2e
tls_hostname=10.0.2.2
tls_suffix=$$
tls_container="covalent-android-tls-$tls_suffix"
wrong_tls_container="covalent-android-wrong-tls-$tls_suffix"
tls_data_volume="$tls_container-data"
tls_config_volume="$tls_container-config"
wrong_tls_data_volume="$wrong_tls_container-data"
wrong_tls_config_volume="$wrong_tls_container-config"
tls_directory=""
instrumentation_log=""
expected_suite=""
device_gate_lock="${TMPDIR:-/tmp}/covalent-api37-device-gate.lock"
lock_acquired=false

cleanup() {
  docker rm -f "$tls_container" "$wrong_tls_container" >/dev/null 2>&1 || true
  docker volume rm \
    "$tls_data_volume" \
    "$tls_config_volume" \
    "$wrong_tls_data_volume" \
    "$wrong_tls_config_volume" >/dev/null 2>&1 || true
  if [ -n "$tls_directory" ] && [ -d "$tls_directory" ]; then
    rm -rf "$tls_directory"
  fi
  if [ -n "$instrumentation_log" ] && [ -f "$instrumentation_log" ]; then
    rm -f "$instrumentation_log"
  fi
  if [ -n "$expected_suite" ] && [ -f "$expected_suite" ]; then
    rm -f "$expected_suite"
  fi
  if [ "$lock_acquired" = true ]; then
    rmdir "$device_gate_lock" 2>/dev/null || true
  fi
}
trap cleanup EXIT INT TERM

# Derive what this device run has to prove before spending forty minutes on an
# emulator. The expectation is the set of @Test methods in src/androidTest, so
# it tracks the tree instead of a constant that goes stale on the next commit,
# and an unparseable suite fails here with a file and line rather than at the
# end of the run.
expected_suite=$(mktemp "${TMPDIR:-/tmp}/covalent-api37-suite.XXXXXX")
if ! derive_android_instrumentation_suite \
  "$repo_root/apps/android/app/src/androidTest" > "$expected_suite"; then
  echo "Could not determine which instrumentation tests this gate must prove." >&2
  exit 1
fi
echo "API 37 device gate must prove $(grep -c '^' "$expected_suite") named tests:"
sed 's/^/  /' "$expected_suite"

if [ -z "$android_sdk" ] && [ -d "${HOME}/Library/Android/sdk" ]; then
  android_sdk="${HOME}/Library/Android/sdk"
fi
if [ -z "$android_sdk" ]; then
  echo "Android SDK not found; set ANDROID_HOME or ANDROID_SDK_ROOT." >&2
  exit 1
fi

adb="$android_sdk/platform-tools/adb"
if [ ! -x "$adb" ]; then
  echo "adb is missing from $android_sdk/platform-tools." >&2
  exit 1
fi
if [ "$headless_ci" != true ] && ! command -v mobilecli >/dev/null 2>&1; then
  echo "mobilecli is required for headed API 37 evidence." >&2
  exit 1
fi
if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
  echo "Docker is required for the packaged Caddy native TLS gate." >&2
  exit 1
fi
if ! command -v openssl >/dev/null 2>&1 || ! command -v curl >/dev/null 2>&1; then
  echo "openssl and curl are required for the packaged Caddy native TLS gate." >&2
  exit 1
fi

if [ -z "$serial" ]; then
  echo "Set ANDROID_SERIAL=$required_serial and start the exact $avd_name emulator before running this gate." >&2
  echo "Available mobilecli devices:" >&2
  mobilecli devices --platform android --include-offline >&2 || true
  exit 1
fi
if [ "$serial" != "$required_serial" ]; then
  echo "Refusing Android serial $serial; this gate is pinned to $required_serial." >&2
  exit 1
fi
if [ "$("$adb" -s "$serial" get-state 2>/dev/null || true)" != "device" ]; then
  echo "$serial is not online. Start $avd_name on port 5570 and keep its headed window visible." >&2
  exit 1
fi

running_avd=$("$adb" -s "$serial" emu avd name 2>/dev/null | sed -n '1p' | tr -d '\r')
if [ "$running_avd" != "$avd_name" ]; then
  echo "$serial is $running_avd, not required AVD $avd_name." >&2
  exit 1
fi

api_level=$("$adb" -s "$serial" shell getprop ro.build.version.sdk | tr -d '\r')
if [ "$api_level" != "37" ]; then
  echo "$avd_name is running API $api_level; API 37 is required." >&2
  exit 1
fi

# Only one installer/instrumentation session may operate this physical AVD.
# Concurrent package updates can kill Android's active runner and leave AMS
# reporting a misleading target-process attachment failure.
if ! mkdir "$device_gate_lock" 2>/dev/null; then
  echo "Android API 37 device gate is already running ($device_gate_lock); wait for it to finish." >&2
  exit 1
fi
lock_acquired=true
activity_dump=$("$adb" -s "$serial" shell dumpsys activity 2>/dev/null || true)
if printf '%s\n' "$activity_dump" | grep -Eq 'life\\.michaelwong\\.covalent(\\.test)?.*mFinished=false|mFinished=false.*life\\.michaelwong\\.covalent(\\.test)?'; then
  echo "Refusing to install over an active Covalent instrumentation session on $serial." >&2
  exit 1
fi

if [ "$headless_ci" != true ]; then
  mobilecli device info --device "$avd_name"
fi
# Rebuilding here is what kills the guest.
#
# check-android.sh and the docker build below are cold Rust compiles. Even with
# every cache warm they saturate a 4-vCPU CI runner for over a minute, and the
# API 37 guest does not survive being descheduled through it: run 32499985083's
# `adb install` died inside StorageManagerService on a null
# PackageManagerInternal 68 seconds into exactly this load, having accepted an
# identical install moments before it started.
#
# So a caller that has already produced these artifacts *before the guest was
# launched* - which is what ci.yml's pre-emulator-launch-script does - says so
# with COVALENT_ANDROID_PREBUILT=true, and this skips the rebuild only. Nothing
# stops being checked: check-android.sh --verify-prebuilt proves each artifact
# exists and is newer than every source it is built from, re-reads the unit test
# and lint reports to confirm they were a clean pass, and runs the
# instrumentation-result contract battery unchanged; the image check below is
# the same freshness proof for the container. Unset - a developer running this
# script by hand - takes the full build-then-test path exactly as before.
if [ "$prebuilt" = true ]; then
  "$repo_root/scripts/check-android.sh" --verify-prebuilt

  # The image has to prove it came from this source too, and it already carries
  # the means: packaging/docker/Dockerfile stamps ARG VCS_REF into
  # org.opencontainers.image.revision, which is the same label
  # scripts/check-container-contract.sh verifies in the container jobs. Read it
  # back and require the commit this checkout is on. An image built without
  # --build-arg VCS_REF is labelled "unknown" and is rejected here by name,
  # which is the correct answer: an unlabelled image cannot be shown to match.
  if ! image_revision=$(docker image inspect \
    --format '{{index .Config.Labels "org.opencontainers.image.revision"}}' \
    "$tls_image" 2>/dev/null); then
    echo "COVALENT_ANDROID_PREBUILT=true but $tls_image does not exist." >&2
    echo "build it first: docker build --file packaging/docker/Dockerfile --build-arg VCS_REF=\$(git rev-parse HEAD) --tag $tls_image ." >&2
    exit 1
  fi
  head_revision=$(git -C "$repo_root" rev-parse HEAD)
  if [ "$image_revision" != "$head_revision" ]; then
    echo "$tls_image records revision '$image_revision' but this checkout is $head_revision." >&2
    echo "the prebuilt image does not match this source; rebuild it with --build-arg VCS_REF=$head_revision" >&2
    exit 1
  fi
  echo "  current: $tls_image (revision $image_revision)"
else
  "$repo_root/scripts/check-android.sh"

  docker build \
    --file "$repo_root/packaging/docker/Dockerfile" \
    --tag "$tls_image" \
    "$repo_root"
fi

for volume in \
  "$tls_data_volume" \
  "$tls_config_volume" \
  "$wrong_tls_data_volume" \
  "$wrong_tls_config_volume"
do
  docker volume create "$volume" >/dev/null
done

docker run --detach \
  --name "$tls_container" \
  --init \
  --read-only \
  --user 65532:65532 \
  --tmpfs /tmp:size=64m,mode=1777 \
  --mount "type=volume,source=$tls_data_volume,target=/data" \
  --mount "type=volume,source=$tls_config_volume,target=/config" \
  --security-opt no-new-privileges:true \
  --cap-drop ALL \
  --env COVALENT_LISTEN=127.0.0.1:8787 \
  --env COVALENT_PEER_LISTEN=0.0.0.0:8787 \
  --env COVALENT_HTTPS_HOST="$tls_hostname" \
  --env COVALENT_DATA_DIR=/data \
  --env COVALENT_CONFIG_DIR=/config \
  --env 'COVALENT_DEVICE_NAME=Android TLS node' \
  --env COVALENT_LAN_DISCOVERY=false \
  --publish 127.0.0.1::8443/tcp \
  "$tls_image" >/dev/null

docker run --detach \
  --name "$wrong_tls_container" \
  --init \
  --read-only \
  --user 65532:65532 \
  --tmpfs /tmp:size=64m,mode=1777 \
  --mount "type=volume,source=$wrong_tls_data_volume,target=/data" \
  --mount "type=volume,source=$wrong_tls_config_volume,target=/config" \
  --security-opt no-new-privileges:true \
  --cap-drop ALL \
  --env COVALENT_LISTEN=127.0.0.1:8787 \
  --env COVALENT_PEER_LISTEN=0.0.0.0:8787 \
  --env COVALENT_HTTPS_HOST="$tls_hostname" \
  --env COVALENT_DATA_DIR=/data \
  --env COVALENT_CONFIG_DIR=/config \
  --env 'COVALENT_DEVICE_NAME=Wrong TLS node' \
  --env COVALENT_LAN_DISCOVERY=false \
  "$tls_image" >/dev/null

tls_directory=$(mktemp -d "${TMPDIR:-/tmp}/covalent-android-tls.XXXXXX")
for container in "$tls_container" "$wrong_tls_container"; do
  attempt=0
  until docker exec "$container" test -f /config/caddy/data/caddy/pki/authorities/local/root.crt; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 30 ] || [ "$(docker inspect -f '{{.State.Running}}' "$container" 2>/dev/null || true)" != true ]; then
      docker logs "$container" >&2 || true
      echo "Packaged Caddy did not create its private CA." >&2
      exit 1
    fi
    sleep 1
  done
done

docker cp \
  "$tls_container:/config/caddy/data/caddy/pki/authorities/local/root.crt" \
  "$tls_directory/root.crt" >/dev/null
docker cp \
  "$wrong_tls_container:/config/caddy/data/caddy/pki/authorities/local/root.crt" \
  "$tls_directory/wrong-root.crt" >/dev/null
tls_port=$(docker port "$tls_container" 8443/tcp | sed -n '1s/.*://p')
if [ -z "$tls_port" ]; then
  echo "Docker did not publish the packaged Caddy port." >&2
  exit 1
fi

attempt=0
until curl \
  --fail \
  --silent \
  --show-error \
  --noproxy '*' \
  --cacert "$tls_directory/root.crt" \
  --resolve "$tls_hostname:$tls_port:127.0.0.1" \
  "https://$tls_hostname:$tls_port/api/v1/status" >/dev/null 2>&1
do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 30 ]; then
    docker logs "$tls_container" >&2 || true
    echo "Packaged Caddy did not become ready." >&2
    exit 1
  fi
  sleep 1
done

tls_token=$(docker exec "$tls_container" sh -c 'cat /data/local-api-token')
tls_ca=$(openssl base64 -A -in "$tls_directory/root.crt")
wrong_tls_ca=$(openssl base64 -A -in "$tls_directory/wrong-root.crt")
tls_pin=$(
  openssl s_client \
    -connect "127.0.0.1:$tls_port" \
    -servername "$tls_hostname" \
    -showcerts </dev/null 2>/dev/null |
    openssl x509 -outform DER |
    openssl dgst -sha256 -r |
    awk '{print $1}'
)
if [ -z "$tls_token" ]; then
  echo "Packaged node did not create a local API token." >&2
  exit 1
fi
case "$tls_pin" in
  *[!0-9a-f]*|'') tls_pin_valid=false ;;
  *) tls_pin_valid=true ;;
esac
if [ "${#tls_pin}" -ne 64 ] || [ "$tls_pin_valid" != true ]; then
  echo "Could not derive the exact packaged Caddy leaf certificate pin." >&2
  exit 1
fi

# The same reasoning as the build block above, one layer down: both of these
# artifacts were already produced and then proved current by
# check-android.sh --verify-prebuilt, and nothing between there and here writes
# to them. Re-entering Gradle only to be told UP-TO-DATE still costs a
# daemon-less JVM configuration pass on the runner the guest is sharing.
if [ "$prebuilt" != true ]; then
  env \
    ANDROID_HOME="$android_sdk" \
    ANDROID_SDK_ROOT="$android_sdk" \
    ANDROID_SERIAL="$serial" \
    "$repo_root/apps/android/gradlew" \
    -p "$repo_root/apps/android" \
    --no-daemon \
    assembleDebug \
    assembleDebugAndroidTest
fi

apk="$repo_root/apps/android/app/build/outputs/apk/debug/app-debug.apk"
test_apk="$repo_root/apps/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk"
# When instrumentation dies the message `am` prints is the symptom, never the
# cause: a DeadObjectException on the ActivityManager binder says only that
# system_server was gone by the time the call landed, and an
# INSTRUMENTATION_ABORTED says only that it went away mid-run. The cause is in
# the guest's own log, and logd is a separate process that outlives
# system_server, so the buffer is still readable after the crash. Dump it before
# giving up so a red run explains itself instead of needing another red run to
# reproduce. Everything here is post-mortem and best-effort: it runs only on a
# path that has already decided to exit 1, it never changes the exit code, and
# it adds no load while the tests are actually running.
dump_guest_failure_evidence() {
  echo "--- API-37-gate post-mortem: guest death evidence ---" >&2
  echo "--- host load at failure ---" >&2
  { nproc; uptime; free -m; } >&2 2>/dev/null || true
  echo "--- guest /proc/loadavg ---" >&2
  "$adb" -s "$serial" shell cat /proc/loadavg >&2 2>/dev/null || true
  echo "--- guest memory ---" >&2
  "$adb" -s "$serial" shell cat /proc/meminfo 2>/dev/null | head -8 >&2 || true
  # The Watchdog signature is the thing to look for: "WATCHDOG KILLING SYSTEM
  # PROCESS" names the monitored lock that blocked, which identifies precisely
  # which resource starved.
  echo "--- guest logcat: watchdog / crash / lmk signatures ---" >&2
  "$adb" -s "$serial" logcat -d -b main,system,crash -t 4000 2>/dev/null \
    | grep -Ei 'watchdog|killing system process|blocked in handler|am_crash|am_proc_died|system_server|lowmemorykiller|lmkd|Slow operation|ANR in' >&2 \
    || echo "(no watchdog/crash signature matched in the guest log)" >&2
  echo "--- guest logcat: last 120 lines verbatim ---" >&2
  "$adb" -s "$serial" logcat -d -b main,system,crash -t 120 >&2 2>/dev/null || true
  dump_component_resolution_evidence
  echo "--- end API-37-gate post-mortem ---" >&2
}

# The other way this gate goes red is not a dead guest at all: the run completes
# and reports failures of the form
#
#   Unable to resolve activity for: Intent { cmp=life.michaelwong.covalent.test/
#     androidx.activity.ComponentActivity }
#
# That intent is not something this repo constructs, and the `.test` package in
# it is not a typo - it is androidx.test's documented fallback. The default
# method androidx.test.internal.platform.app.ActivityInvoker#getIntentForActivity
# (monitor-1.8.0) compiles to exactly this:
#
#   intent = Intent.makeMainActivity(
#       new ComponentName(getInstrumentation().getTargetContext(), activityClass));
#   if (getInstrumentation().getTargetContext().getPackageManager()
#           .resolveActivity(intent, 0) != null) {
#     return intent;                                  // -> app package
#   }
#   return Intent.makeMainActivity(
#       new ComponentName(getInstrumentation().getContext(), activityClass));
#                                                     // -> TEST package
#
# So a `.test/androidx.activity.ComponentActivity` component name is proof that
# `resolveActivity(intent, 0)` returned null for the app package. The stub is
# supplied by `debugImplementation("androidx.compose.ui:ui-test-manifest")` and
# is verifiably present in app-debug.apk's merged manifest, so a build-output
# check cannot tell us anything more - the question is only ever what the guest's
# PackageManager answered at that instant, and there are exactly four ways for
# that answer to be null:
#
#   1. the component is genuinely absent from the installed app package
#   2. the app package is disabled or not installed for user 0 (flags=0 does not
#      carry MATCH_DISABLED_COMPONENTS)
#   3. user 0's credential-encrypted key is locked, so PackageManager's
#      updateFlagsForComponent narrows an unflagged query to
#      MATCH_DIRECT_BOOT_AWARE only, and this stub is not directBootAware
#   4. package-visibility filtering hides the target from the caller
#
# Reason 3 is the one that also explains the credential-encrypted-storage
# failures and an in-process UserManager.isUserUnlocked() of false in the same
# run, and it is the one nothing here has ever actually measured. `am
# get-started-user-state` and the non-encryption-aware service count both read
# ActivityManager's UserController lifecycle state; the predicate PackageManager
# and UserManager use is StorageManagerService's CE-key set, which is a
# different field in a different service and is only visible in `dumpsys mount`.
# Print both, side by side, so a red run says which of the four it was instead of
# leaving it to be inferred. Same post-mortem discipline as above: read-only,
# best-effort, only on a path already exiting 1.
dump_component_resolution_evidence() {
  stub_activity=androidx.activity.ComponentActivity
  echo "--- API-37-gate post-mortem: ComponentActivity resolution evidence ---" >&2

  echo "--- installed packages and UIDs ---" >&2
  "$adb" -s "$serial" shell pm list packages -U 2>/dev/null \
    | grep -i covalent >&2 || echo "(no covalent package is installed)" >&2
  echo "--- packages currently disabled ---" >&2
  "$adb" -s "$serial" shell pm list packages -d 2>/dev/null \
    | grep -i covalent >&2 || echo "(no covalent package is disabled)" >&2

  # Reason 2: per-user install/enable/stopped state for both packages.
  for package in life.michaelwong.covalent life.michaelwong.covalent.test; do
    echo "--- $package: user 0 package state ---" >&2
    "$adb" -s "$serial" shell dumpsys package "$package" 2>/dev/null \
      | grep -E 'User 0:|enabledComponents|disabledComponents' >&2 \
      || echo "(no user-0 state reported for $package)" >&2
  done

  # Reason 1: is the stub actually on the device, as opposed to merely present in
  # a merged manifest at build time? --all-components is what makes dumpsys emit
  # activity entries for a named package at all.
  for package in life.michaelwong.covalent life.michaelwong.covalent.test; do
    echo "--- $package: on-device $stub_activity declaration ---" >&2
    "$adb" -s "$serial" shell dumpsys package --all-components "$package" 2>/dev/null \
      | grep -A5 "$stub_activity" >&2 \
      || echo "($package does not declare $stub_activity on-device)" >&2
  done

  # The predicate itself, run the same way ActivityInvoker runs it: an explicit
  # component name, no match flags. This is the single most direct reading
  # available of what the failing call saw.
  for package in life.michaelwong.covalent life.michaelwong.covalent.test; do
    echo "--- resolve-activity (explicit component) $package/$stub_activity ---" >&2
    "$adb" -s "$serial" shell cmd package resolve-activity --brief --user 0 \
      -n "$package/$stub_activity" >&2 2>&1 || true
  done
  for package in life.michaelwong.covalent life.michaelwong.covalent.test; do
    echo "--- resolve-activity (MAIN/LAUNCHER) $package ---" >&2
    "$adb" -s "$serial" shell cmd package resolve-activity --brief --user 0 \
      -a android.intent.action.MAIN \
      -c android.intent.category.LAUNCHER "$package" >&2 2>&1 || true
  done

  # Reason 3, the measurement nothing upstream takes. "CE unlocked users" is
  # StorageManagerService's own list and is the field UserManager.isUserUnlocked()
  # and PackageManager's direct-boot narrowing both consult. If it omits 0 while
  # am reports RUNNING_UNLOCKED, that is the whole failure, and the two lines
  # printed together are the proof rather than an inference.
  echo "--- StorageManagerService CE/DE key state (the isUserUnlocked predicate) ---" >&2
  "$adb" -s "$serial" shell dumpsys mount 2>/dev/null \
    | grep -iE 'unlocked users|unlocked' >&2 \
    || echo "(dumpsys mount reported no unlocked-user state)" >&2
  echo "--- ActivityManager user lifecycle state (a different field) ---" >&2
  "$adb" -s "$serial" shell am get-started-user-state 0 >&2 2>&1 || true
  "$adb" -s "$serial" shell cmd user list -v >&2 2>&1 || true
  "$adb" -s "$serial" shell dumpsys user 2>/dev/null \
    | grep -E 'id=0,|State: |Started users state|Unlock time' >&2 || true

  # Reason 4 needs the instrumentation's own wiring: a targetPackage that is not
  # the app package would mean getTargetContext() was never the app to begin with.
  echo "--- declared instrumentation and targetPackage ---" >&2
  "$adb" -s "$serial" shell pm list instrumentation 2>/dev/null \
    | grep -i covalent >&2 || echo "(no covalent instrumentation is registered)" >&2
  "$adb" -s "$serial" shell dumpsys package life.michaelwong.covalent.test 2>/dev/null \
    | grep -B2 -A6 -i 'instrumentation' >&2 || true

  echo "--- guest logcat: activity-resolution signatures ---" >&2
  "$adb" -s "$serial" logcat -d -b main,system,crash -t 4000 2>/dev/null \
    | grep -Ei 'Unable to resolve activity|ActivityNotFound|user [0-9]+ is (still )?locked|not directBootAware|direct.?boot|credential.encrypted|CE storage|isUserUnlocked|unlockUser|onUserUnlock' >&2 \
    || echo "(no activity-resolution signature matched in the guest log)" >&2
}

# Installing is the heaviest thing this gate asks of the guest, and it is where
# run 32520518338 actually died:
#
#   API-37-gate: probe install accepted after 16s
#   API-37-gate: binder service 'package' is gone at 38s; restarting the window
#   API-37-gate: guest ready and stable across 4 samples after 60s
#   adb: failed to install .../app-debug.apk:
#     cmd: Failure calling service package: Broken pipe (32)
#
# That is a young guest whose package service is flapping, not a locked user -
# a different fault from the 40-failure shape, and one this script reported in a
# single line because `adb install` ran bare under `set -e`. Nothing dumped, so
# the next diagnosis started from that one line again. Route these through the
# same post-mortem the instrumentation paths use: the guest's own log is the
# only place the reason for a vanished `package` service is written down, and
# logd outlives system_server, so it is still readable afterwards.
install_or_dump() {
  if ! "$adb" -s "$serial" install -r "$1" >/dev/null; then
    echo "Android install of $1 failed on $serial." >&2
    dump_guest_failure_evidence
    exit 1
  fi
}
if [ "$headless_ci" = true ]; then
  install_or_dump "$apk"
else
  mobilecli apps install "$apk" --device "$avd_name"
fi
install_or_dump "$test_apk"
# Android 17 installs instrumentation packages disabled. Enable the exact
# package before launch so the device gate cannot silently report zero tests.
"$adb" -s "$serial" shell pm enable life.michaelwong.covalent.test >/dev/null

# The post-mortem above is a post-mortem: it runs after `am instrument` returns,
# which on a 56-test suite is tens of minutes after the first test failed. A
# guest that was credential-encrypted-locked at t=0 and recovered by the time
# the dump runs reads perfectly healthy there, and that is precisely the shape
# this gate has been failing in. So take the same readings once, unconditionally,
# at the instant they govern - immediately before instrumentation starts.
#
# Four adb calls on a guest that is about to be handed a 56-test suite; the cost
# is not measurable against what follows, and unlike the post-mortem this cannot
# be too late.
# surfaceflinger's and system_server's pids, read as one pair so they can be
# compared as one pair. This gate's long-standing fault was an abort loop -
# RegionSamplingThread trips `!hasReadColorBufferDma`, surfaceflinger dies,
# system_server follows, the framework restarts every ~29s - and the loop leaves
# gaps wide enough for a whole readiness window, so it has been possible for this
# gate to look healthy at every instant it was asked. Reading these before and
# after instrumentation is what makes "the suite passed" mean "the suite passed
# on one stable guest" instead of "the suite passed between two restarts".
framework_pids() {
  "$adb" -s "$serial" shell ps -A -o PID,NAME 2>/dev/null | tr -d '\r' |
    awk '$2 == "surfaceflinger" { sf = $1 }
         $2 == "system_server" { ss = $1 }
         END { printf "surfaceflinger=%s system_server=%s", (sf ? sf : "unreadable"), (ss ? ss : "unreadable") }'
}

echo "--- API-37-gate: guest state at instrumentation start ---"
printf 'CE key set        : '
"$adb" -s "$serial" shell dumpsys mount 2>/dev/null \
  | grep -m1 -i 'CE unlocked users' || echo "(unreported)"
printf 'user 0 lifecycle  : '
"$adb" -s "$serial" shell am get-started-user-state 0 2>&1 || true
printf 'ComponentActivity : '
"$adb" -s "$serial" shell cmd package resolve-activity --brief --user 0 \
  -n life.michaelwong.covalent/androidx.activity.ComponentActivity 2>&1 | tail -1
printf 'app package state : '
"$adb" -s "$serial" shell dumpsys package life.michaelwong.covalent 2>/dev/null \
  | grep -m1 -o 'installed=true[^,]*enabled=[0-9]*' || echo "(unreported)"
printf 'framework pids    : '
framework_pids_before=$(framework_pids)
echo "$framework_pids_before"
echo "--- end guest state ---"

instrumentation_log=$(mktemp "${TMPDIR:-/tmp}/covalent-api37-instrumentation.XXXXXX")
if ! "$adb" -s "$serial" shell am instrument -w -r \
  -e covalentTlsBaseUrl "https://$tls_hostname:$tls_port" \
  -e covalentTlsToken "$tls_token" \
  -e covalentTlsCa "$tls_ca" \
  -e covalentTlsWrongCa "$wrong_tls_ca" \
  -e covalentTlsPin "$tls_pin" \
  life.michaelwong.covalent.test/androidx.test.runner.AndroidJUnitRunner >"$instrumentation_log" 2>&1; then
  cat "$instrumentation_log" >&2
  echo "Android instrumentation command failed on $serial." >&2
  dump_guest_failure_evidence
  exit 1
fi
cat "$instrumentation_log"
if ! validate_android_api37_result "$instrumentation_log" "$expected_suite"; then
  echo "Android instrumentation result is invalid on $serial." >&2
  dump_guest_failure_evidence
  exit 1
fi

# The suite passed. Now say whether it passed on the guest it started on.
#
# scripts/disable-android-guest-launcher.sh removes the CompositionSamplingListener
# registrant before this run, and scripts/wait-for-android-guest.sh proves the
# framework held across its stability window; this closes the remaining gap, which
# is the tens of minutes in between. A framework restart here is the abort loop
# still running, and a run that reports success while it is running would send
# exactly the wrong lane green. Reported as a failure with both readings named,
# not as a warning, because it means the result above was taken on two guests.
#
# Only compared when both readings are readable: a `ps` that did not come back is
# not evidence of a restart, and this check is not allowed to invent one.
framework_pids_after=$(framework_pids)
case "$framework_pids_before$framework_pids_after" in
  *unreadable*)
    echo "Framework pids were not readable on both sides of instrumentation (before: $framework_pids_before, after: $framework_pids_after); not compared." >&2
    ;;
  *)
    if [ "$framework_pids_after" != "$framework_pids_before" ]; then
      echo "The framework restarted during instrumentation on $serial." >&2
      echo "  before: $framework_pids_before" >&2
      echo "  after : $framework_pids_after" >&2
      echo "surfaceflinger aborting in RegionSamplingThread is what does this; the suite result above was taken across a restart." >&2
      dump_guest_failure_evidence
      exit 1
    fi
    echo "API-37-gate: framework held across instrumentation: $framework_pids_after"
    ;;
esac
"$adb" -s "$serial" shell pm clear life.michaelwong.covalent >/dev/null
if [ "$headless_ci" = true ]; then
  "$adb" -s "$serial" shell am start -W -n life.michaelwong.covalent/.MainActivity >/dev/null
elif ! mobilecli apps launch life.michaelwong.covalent --device "$avd_name"; then
  echo "mobilecli launch backend is incompatible with this API 37 image; using explicit adb lifecycle fallback." >&2
  "$adb" -s "$serial" shell am start -W -n life.michaelwong.covalent/.MainActivity
fi

evidence_dir=${COVALENT_ANDROID_EVIDENCE_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/covalent-api37.XXXXXX")}
mkdir -p "$evidence_dir"
if [ "$headless_ci" = true ]; then
  "$adb" -s "$serial" exec-out screencap -p > "$evidence_dir/first-launch.png"
  "$adb" -s "$serial" shell uiautomator dump /sdcard/covalent-ui.xml >/dev/null
  "$adb" -s "$serial" pull /sdcard/covalent-ui.xml "$evidence_dir/first-launch-ui.xml" >/dev/null
else
  mobilecli screenshot \
    --device "$avd_name" \
    --format png \
    --output "$evidence_dir/first-launch.png"
  mobilecli dump ui --device "$avd_name" > "$evidence_dir/first-launch-ui.txt"
fi

"$adb" -s "$serial" reverse tcp:8787 tcp:8787
echo "API 37 device gate passed on $serial ($avd_name)."
echo "Evidence: $evidence_dir"
echo "ADB reverse is ready: Android http://127.0.0.1:8787 -> host 127.0.0.1:8787."
