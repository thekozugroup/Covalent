#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
. "$repo_root/scripts/android-instrumentation-result.sh"
android_sdk=${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}
avd_name=Covalent_API_37
required_serial=emulator-5570
serial=${ANDROID_SERIAL:-}
headless_ci=${COVALENT_ANDROID_HEADLESS_CI:-false}
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
"$repo_root/scripts/check-android.sh"

docker build \
  --file "$repo_root/packaging/docker/Dockerfile" \
  --tag "$tls_image" \
  "$repo_root"

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

env \
  ANDROID_HOME="$android_sdk" \
  ANDROID_SDK_ROOT="$android_sdk" \
  ANDROID_SERIAL="$serial" \
  "$repo_root/apps/android/gradlew" \
  -p "$repo_root/apps/android" \
  --no-daemon \
  assembleDebug \
  assembleDebugAndroidTest

apk="$repo_root/apps/android/app/build/outputs/apk/debug/app-debug.apk"
test_apk="$repo_root/apps/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk"
if [ "$headless_ci" = true ]; then
  "$adb" -s "$serial" install -r "$apk" >/dev/null
else
  mobilecli apps install "$apk" --device "$avd_name"
fi
"$adb" -s "$serial" install -r "$test_apk" >/dev/null
# Android 17 installs instrumentation packages disabled. Enable the exact
# package before launch so the device gate cannot silently report zero tests.
"$adb" -s "$serial" shell pm enable life.michaelwong.covalent.test >/dev/null
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
  exit 1
fi
cat "$instrumentation_log"
if ! validate_android_api37_result "$instrumentation_log" "$expected_suite"; then
  echo "Android instrumentation result is invalid on $serial." >&2
  exit 1
fi
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
