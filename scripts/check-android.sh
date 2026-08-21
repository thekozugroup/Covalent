#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
android_java=""
android_sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"

# This script is two things joined together: a Gradle invocation that *builds*
# (assembleDebug/Release/DebugAndroidTest) and runs the JVM-side verification
# (test, lint), and a source-derived contract battery that *checks* and touches
# no build system at all. Callers that have already run the build half - see
# scripts/android-api37-device-test.sh, which must not saturate a CI runner
# while an emulator guest is booted - ask for `--verify-prebuilt`. That mode
# skips the rebuild and nothing else: it proves each artifact the rebuild would
# have produced is present and newer than every source it is built from, and
# re-reads the unit test and lint reports to confirm they were a clean pass, so
# a stale or failed prebuild fails here instead of being trusted. The contract
# battery at the bottom runs in both modes, unconditionally.
mode=full
if [ "${1:-}" = "--verify-prebuilt" ]; then
  mode=verify-prebuilt
  shift
fi
if [ "$#" -ne 0 ]; then
  echo "usage: check-android.sh [--verify-prebuilt]" >&2
  exit 1
fi

if [ "$(uname -s)" = "Darwin" ]; then
  android_java=$(/usr/libexec/java_home -v 21 2>/dev/null || true)
  if [ -z "$android_java" ]; then
    android_java=$(/usr/libexec/java_home -v 17 2>/dev/null || true)
  fi
fi

if [ -z "$android_sdk" ] && [ -d "${HOME}/Library/Android/sdk" ]; then
  android_sdk="${HOME}/Library/Android/sdk"
fi

if [ -z "$android_sdk" ]; then
  echo "Android SDK not found; set ANDROID_HOME or ANDROID_SDK_ROOT." >&2
  exit 1
fi

# `assembleRelease` depends on `:app:buildAndroidJni`, which shells out to
# scripts/build-android-jni.sh and hard-requires COVALENT_ANDROID_NDK_HOME. No
# caller ever set it, so this gate could not pass anywhere. Resolve the pinned
# NDK out of the Android SDK the same way the JDK and SDK are resolved above,
# keeping scripts/build-android-jni.sh as the single source of truth for the
# pinned version, and fail closed with actionable instructions when it is absent.
ndk_version=$(sed -n 's/^ndk_version=\([0-9.]*\)$/\1/p' "$repo_root/scripts/build-android-jni.sh")
if [ -z "$ndk_version" ]; then
  echo "unable to read the pinned NDK version from scripts/build-android-jni.sh" >&2
  exit 1
fi

android_ndk="${COVALENT_ANDROID_NDK_HOME:-$android_sdk/ndk/$ndk_version}"
if [ ! -d "$android_ndk" ]; then
  echo "Android NDK $ndk_version is required at $android_ndk" >&2
  echo "install it with: sdkmanager \"ndk;$ndk_version\"" >&2
  echo "or set COVALENT_ANDROID_NDK_HOME to an existing NDK $ndk_version directory" >&2
  exit 1
fi

# Everything the Gradle half consumes. `assembleRelease` reaches
# `:app:buildAndroidJni`, which cross-compiles the Rust crates through the NDK,
# so the crates and the Cargo lockfile are build inputs of the Android artifacts
# exactly as much as apps/android is.
android_build_inputs="apps/android crates Cargo.toml Cargo.lock scripts/build-android-jni.sh"
android_prebuild_stamp="$repo_root/apps/android/app/build/covalent-prebuild-stamp"

# Fingerprint the source these artifacts are built from: the commit, plus the
# uncommitted state of the input paths. Deliberately *not* an mtime comparison.
# Gradle decides staleness by content, so a file rewritten with identical bytes
# - by a rebase, a checkout, or another tool - is not a rebuild trigger for
# Gradle and must not be one here either, or this mode would reject artifacts
# that are genuinely correct. `git status --porcelain` honours .gitignore, so
# the generated build tree is excluded without having to be pruned by hand, and
# it is index-cached, so this costs milliseconds rather than hashing the tree.
android_checkout_is_git() {
  command -v git >/dev/null 2>&1 && git -C "$repo_root" rev-parse --git-dir >/dev/null 2>&1
}

# Callers must gate this on android_checkout_is_git first: it is used inside a
# pipeline below, where an `exit` would only leave the subshell.
android_checkout_fingerprint() {
  git -C "$repo_root" rev-parse HEAD
  # Word splitting is the point - $android_build_inputs is a list of pathspecs.
  # shellcheck disable=SC2086
  git -C "$repo_root" status --porcelain -- $android_build_inputs
}

require_prebuilt_artifact() {
  require_label=$1
  require_path=$2
  if [ ! -e "$require_path" ]; then
    echo "$require_label is missing at $require_path" >&2
    echo "--verify-prebuilt requires a prior full ./scripts/check-android.sh in this checkout" >&2
    exit 1
  fi
  echo "  present: $require_label"
}

if [ "$mode" = verify-prebuilt ]; then
  echo "check-android: verifying prebuilt Android artifacts instead of rebuilding them"

  if ! android_checkout_is_git; then
    echo "--verify-prebuilt needs a git checkout to prove the artifacts match this source" >&2
    echo "run the full ./scripts/check-android.sh instead" >&2
    exit 1
  fi
  # The stamp is the only thing that ties the artifacts below to this source.
  # Without it, "the APK exists" is a statement about some earlier checkout.
  if [ ! -f "$android_prebuild_stamp" ]; then
    echo "no prebuild stamp at $android_prebuild_stamp" >&2
    echo "--verify-prebuilt requires a prior full ./scripts/check-android.sh in this checkout" >&2
    exit 1
  fi
  if ! android_checkout_fingerprint | cmp -s - "$android_prebuild_stamp"; then
    echo "the prebuilt Android artifacts were built from different source than this checkout" >&2
    echo "recorded at prebuild time:" >&2
    sed 's/^/  /' "$android_prebuild_stamp" >&2
    echo "this checkout now:" >&2
    android_checkout_fingerprint | sed 's/^/  /' >&2
    echo "rerun the full ./scripts/check-android.sh" >&2
    exit 1
  fi
  echo "  current: artifacts match this checkout ($(sed -n '1p' "$android_prebuild_stamp"))"

  require_prebuilt_artifact "debug APK" \
    "$repo_root/apps/android/app/build/outputs/apk/debug/app-debug.apk"
  require_prebuilt_artifact "instrumentation APK" \
    "$repo_root/apps/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk"
  require_prebuilt_artifact "release APK" \
    "$repo_root/apps/android/app/build/outputs/apk/release/app-release-unsigned.apk"
  require_prebuilt_artifact "debug unit test results" \
    "$repo_root/apps/android/app/build/test-results/testDebugUnitTest"
  require_prebuilt_artifact "debug lint report" \
    "$repo_root/apps/android/app/build/reports/lint-results-debug.xml"

  # Presence is not the same as a pass. Read the reports the
  # skipped `test` and `lint` tasks wrote and re-assert their verdicts here, so
  # this path can never trust a prebuild that failed.
  unit_results="$repo_root/apps/android/app/build/test-results/testDebugUnitTest"
  unit_total=$(grep -hoE 'tests="[0-9]+"' "$unit_results"/*.xml 2>/dev/null |
    sed 's/[^0-9]//g' | awk '{total += $1} END {print total + 0}')
  unit_bad=$(grep -hoE '(failures|errors)="[0-9]+"' "$unit_results"/*.xml 2>/dev/null |
    sed 's/[^0-9]//g' | awk '{total += $1} END {print total + 0}')
  if [ "$unit_total" -lt 1 ] || [ "$unit_bad" -ne 0 ]; then
    echo "prebuilt debug unit tests are not a clean pass: $unit_total tests, $unit_bad failures/errors" >&2
    exit 1
  fi
  echo "  verified: $unit_total debug unit tests recorded, 0 failures and 0 errors"

  lint_report="$repo_root/apps/android/app/build/reports/lint-results-debug.xml"
  lint_bad=$(grep -cE 'severity="(Error|Fatal)"' "$lint_report" || true)
  if [ "$lint_bad" -ne 0 ]; then
    echo "prebuilt debug lint report records $lint_bad error-severity issues at $lint_report" >&2
    grep -E 'severity="(Error|Fatal)"' "$lint_report" >&2 || true
    exit 1
  fi
  echo "  verified: debug lint report records no error-severity issues"
elif [ -n "$android_java" ]; then
  env JAVA_HOME="$android_java" ANDROID_HOME="$android_sdk" ANDROID_SDK_ROOT="$android_sdk" COVALENT_ANDROID_NDK_HOME="$android_ndk" "$repo_root/apps/android/gradlew" -p "$repo_root/apps/android" --no-daemon test lint assembleDebug assembleRelease assembleDebugAndroidTest
else
  env ANDROID_HOME="$android_sdk" ANDROID_SDK_ROOT="$android_sdk" COVALENT_ANDROID_NDK_HOME="$android_ndk" "$repo_root/apps/android/gradlew" -p "$repo_root/apps/android" --no-daemon test lint assembleDebug assembleRelease assembleDebugAndroidTest
fi

# Record what this build was made from. Written only after the Gradle run above
# exited 0, so the stamp can never vouch for a failed build, and removed outright
# when there is no checkout to fingerprint so a later --verify-prebuilt cannot
# find a stale one and trust it.
if [ "$mode" = full ]; then
  if android_checkout_is_git; then
    android_checkout_fingerprint > "$android_prebuild_stamp"
  else
    rm -f "$android_prebuild_stamp"
  fi
fi

"$repo_root/scripts/test-android-instrumentation-result.sh"
