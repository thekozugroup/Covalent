#!/bin/sh
set -eu

package=${1:?Pass an APK or AAB path}
zipalign_bin=${COVALENT_ANDROID_ZIPALIGN:?Set COVALENT_ANDROID_ZIPALIGN to the Android SDK zipalign binary}

test -f "$package" || { echo "Missing Android package: $package" >&2; exit 1; }
test -x "$zipalign_bin" || { echo "zipalign is required for package verification" >&2; exit 1; }

case "$package" in
  *.apk) "$zipalign_bin" -c -P 16 4 "$package" ;;
  *.aab) ;;
  *) echo "Package must be an APK or AAB" >&2; exit 1 ;;
esac

test "$(wc -c < "$package")" -le 83886080 || {
  echo "Android package exceeds the 80 MiB release budget" >&2
  exit 1
}
for abi in arm64-v8a x86_64; do
  entry=$(unzip -Z1 "$package" | grep "/lib/$abi/libcovalent_android_jni.so$" || true)
  test -n "$entry" || { echo "Package is missing JNI library for $abi" >&2; exit 1; }
  size=$(unzip -l "$package" "$entry" | awk 'NR == 4 { print $1 }')
  test "${size:-0}" -le 2097152 || {
    echo "Packaged JNI library for $abi exceeds the 2 MiB release budget" >&2
    exit 1
  }
done

echo "Android native package checks passed."
