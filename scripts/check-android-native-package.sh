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
  # `unzip -l` reports the uncompressed Length. The app now uses legacy native
  # packaging so the installer extracts executable helpers into nativeLibraryDir;
  # the per-library budget remains defined over the exact uncompressed ELF bytes.
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

# Verify both executable helpers against the exact manifests packaged beside
# them. This reads archive members without extracting executable files to a
# writable location. Android repeats both hashes before passing installed paths
# to Rust, which then reopens and hashes each object again at launch.
python3 - "$package" \
  "$COVALENT_SYNC_ENGINE_MIN_BYTES" "$COVALENT_SYNC_ENGINE_MAX_BYTES" \
  "$COVALENT_ENGINE_GUARDIAN_MIN_BYTES" "$COVALENT_ENGINE_GUARDIAN_MAX_BYTES" <<'PY'
import hashlib, pathlib, re, sys, zipfile

package = pathlib.Path(sys.argv[1])
worker_min, worker_max, guardian_min, guardian_max = map(int, sys.argv[2:])
prefix = "base/" if package.suffix == ".aab" else ""
abis = ("arm64-v8a", "x86_64")

def parse_manifest(archive, name, minimum, maximum):
    info = archive.getinfo(prefix + "assets/" + name)
    if info.file_size > 1024:
        raise SystemExit(f"{name} exceeds its packaged byte bound")
    raw = archive.read(info)
    try:
        text = raw.decode("ascii")
    except UnicodeDecodeError:
        raise SystemExit(f"{name} is not ASCII")
    if not text.endswith("\n") or "\r" in text:
        raise SystemExit(f"{name} is not canonical")
    rows = {}
    for line in text[:-1].split("\n"):
        parts = line.split(" ")
        if len(parts) != 3 or any(not part for part in parts):
            raise SystemExit(f"{name} has a malformed row")
        abi, digest, size_text = parts
        if abi not in abis or abi in rows or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise SystemExit(f"{name} has an invalid ABI or digest")
        try:
            size = int(size_text)
        except ValueError:
            raise SystemExit(f"{name} has an invalid size")
        if str(size) != size_text or not minimum <= size <= maximum:
            raise SystemExit(f"{name} has a size outside its reviewed bound")
        rows[abi] = (digest, size)
    if set(rows) != set(abis):
        raise SystemExit(f"{name} must contain exactly both supported ABIs")
    return rows

def verify_helpers(archive, library, manifest):
    for abi in abis:
        name = f"{prefix}lib/{abi}/{library}"
        if archive.namelist().count(name) != 1:
            raise SystemExit(f"package must contain exactly one {name}")
        info = archive.getinfo(name)
        expected_hash, expected_size = manifest[abi]
        if info.file_size != expected_size:
            raise SystemExit(f"packaged {library} size differs from its manifest for {abi}")
        digest = hashlib.sha256()
        retained = 0
        with archive.open(info) as source:
            while chunk := source.read(1024 * 1024):
                retained += len(chunk)
                if retained > expected_size:
                    raise SystemExit(f"packaged {library} exceeded its manifest for {abi}")
                digest.update(chunk)
        if retained != expected_size or digest.hexdigest() != expected_hash:
            raise SystemExit(f"packaged {library} hash differs from its manifest for {abi}")

with zipfile.ZipFile(package) as archive:
    if len(archive.namelist()) != len(set(archive.namelist())):
        raise SystemExit("package contains duplicate archive member names")
    workers = parse_manifest(
        archive, "syncthing-sha256.txt", worker_min, worker_max)
    guardians = parse_manifest(
        archive, "engine-guardian-sha256.txt", guardian_min, guardian_max)
    verify_helpers(archive, "libsyncthing.so", workers)
    verify_helpers(archive, "libengineguardian.so", guardians)
PY

echo "Android native package checks passed for $package."
