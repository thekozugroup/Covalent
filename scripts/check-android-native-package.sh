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
  # header, `size` comes back empty, `${size:-0}` substitutes 0, and the 2 MiB
  # budget then passes for a library of any size at all. Match the entry by name
  # instead of by line number, and refuse a size that is not a plain integer
  # rather than defaulting it to something that always passes.
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
      echo "Refusing to compare an unparseable size against the 2 MiB budget." >&2
      exit 1
      ;;
  esac
  test "$size" -le 2097152 || {
    echo "Packaged JNI library for $abi is $size bytes, over the 2 MiB release budget" >&2
    exit 1
  }
  echo "  $abi JNI library: $size bytes (budget 2097152)"
done

echo "Android native package checks passed for $package."
