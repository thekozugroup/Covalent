#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
android_java=""
android_sdk="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-}}"

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

if [ -n "$android_java" ]; then
  env JAVA_HOME="$android_java" ANDROID_HOME="$android_sdk" ANDROID_SDK_ROOT="$android_sdk" COVALENT_ANDROID_NDK_HOME="$android_ndk" "$repo_root/apps/android/gradlew" -p "$repo_root/apps/android" --no-daemon test lint assembleDebug assembleRelease assembleDebugAndroidTest
else
  env ANDROID_HOME="$android_sdk" ANDROID_SDK_ROOT="$android_sdk" COVALENT_ANDROID_NDK_HOME="$android_ndk" "$repo_root/apps/android/gradlew" -p "$repo_root/apps/android" --no-daemon test lint assembleDebug assembleRelease assembleDebugAndroidTest
fi

"$repo_root/scripts/test-android-instrumentation-result.sh"
