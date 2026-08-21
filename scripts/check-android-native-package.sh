#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
# Same floor/ceiling the build gate applies to the freshly linked .so, so the
# packaged copy is held to exactly the number that was derived by measurement.
. "$repo_root/scripts/android-native-budgets.sh"

package=${1:?Pass an APK or AAB path}
zipalign_bin=${COVALENT_ANDROID_ZIPALIGN:?Set COVALENT_ANDROID_ZIPALIGN to the Android SDK zipalign binary}

test -f "$package" || { echo "Missing Android package: $package" >&2; exit 1; }
test -x "$zipalign_bin" || { echo "zipalign is required for package verification" >&2; exit 1; }

case "$package" in
  *.apk) "$zipalign_bin" -c -P 16 4 "$package" ;;
  *.aab) ;;
  *) echo "Package must be an APK or AAB" >&2; exit 1 ;;
esac

package_bytes=$(wc -c < "$package")
test "$package_bytes" -le "$COVALENT_ANDROID_PACKAGE_MAX_BYTES" || {
  echo "Android package is $package_bytes bytes, over the ${COVALENT_ANDROID_PACKAGE_MAX_BYTES}-byte release budget" >&2
  exit 1
}
echo "  package: $package_bytes bytes (budget $COVALENT_ANDROID_PACKAGE_MAX_BYTES)"
for abi in arm64-v8a x86_64; do
  # An APK stores the library at `lib/<abi>/...` and an AAB at `base/lib/<abi>/...`.
  # The previous pattern required a leading slash, so it matched the AAB layout
  # and never the APK one - invisible while nothing invoked this script, fatal
  # the moment it does.
  entries=$(unzip -Z1 "$package" | grep -E "(^|/)lib/$abi/libcovalent_android_jni\.so$" || true)
  test -n "$entries" || { echo "Package is missing JNI library for $abi" >&2; exit 1; }
  if [ "$(printf '%s\n' "$entries" | grep -c '^')" -ne 1 ]; then
    echo "Package has more than one JNI library for $abi:" >&2
    printf '%s\n' "$entries" >&2
    exit 1
  fi
  entry=$entries
  # This used to be `awk 'NR == 4 { print $1 }'` followed by `test "${size:-0}"`.
  # An archive comment, a different unzip build or a localised listing shifts the
  # header, `size` comes back empty, `${size:-0}` substitutes 0, and the size
  # budget then passes for a library of any size at all. Match the entry by name
  # instead of by line number, and refuse a size that is not a plain integer
  # rather than defaulting it to something that always passes.
  # `unzip -l` reports the uncompressed Length, which is the number that matters:
  # AGP stores .so entries uncompressed (minSdk 26, useLegacyPackaging unset), so
  # this equals both the on-disk library size and its cost inside the package.
  size=$(unzip -l "$package" "$entry" | awk -v entry="$entry" '
    $NF == entry { print $1; found = 1; exit }
    END { if (!found) { exit 1 } }
  ') || {
    echo "Could not read the packaged size of $entry from $package" >&2
    exit 1
  }
  case "$size" in
    ''|*[!0-9]*)
      echo "Packaged size of $entry is not a number: '${size}'" >&2
      echo "Refusing to compare an unparseable size against the size budget." >&2
      exit 1
      ;;
  esac
  test "$size" -le "$COVALENT_JNI_MAX_BYTES" || {
    echo "Packaged JNI library for $abi is $size bytes, over the ${COVALENT_JNI_MAX_BYTES}-byte release budget" >&2
    exit 1
  }
  # The ceiling cannot catch a dead-stripped library - an empty one passes every
  # maximum. scripts/android-native-budgets.sh explains the floor; apply it to
  # the packaged copy too, so a stripped runtime cannot reach a signed artefact.
  test "$size" -ge "$COVALENT_JNI_MIN_BYTES" || {
    echo "Packaged JNI library for $abi is only $size bytes, under the ${COVALENT_JNI_MIN_BYTES}-byte floor." >&2
    echo "A library this small cannot contain the node runtime; it has almost certainly been dead-stripped." >&2
    exit 1
  }
  echo "  $abi JNI library: $size bytes (floor $COVALENT_JNI_MIN_BYTES, budget $COVALENT_JNI_MAX_BYTES)"
done

echo "Android native package checks passed for $package."
