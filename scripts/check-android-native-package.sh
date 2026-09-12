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
import hashlib, json, pathlib, re, sys, zipfile

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

def read_bounded(archive, name, maximum):
    full_name = prefix + "assets/" + name
    if archive.namelist().count(full_name) != 1:
        raise SystemExit(f"package must contain exactly one {full_name}")
    info = archive.getinfo(full_name)
    if not 0 < info.file_size <= maximum:
        raise SystemExit(f"packaged {name} is empty or exceeds its bound")
    return archive.read(info)

def verify_notices(archive):
    index = read_bounded(archive, "sync-engine-notices-index.txt", 1024)
    try:
        text = index.decode("ascii")
    except UnicodeDecodeError:
        raise SystemExit("packaged notice index is not ASCII")
    lines = text.splitlines()
    if not text.endswith("\n") or "\r" in text or len(lines) != 3 or lines[0] != "1":
        raise SystemExit("packaged notice index is not canonical")
    expected = {
        "combined": ("sync-engine-notices/THIRD-PARTY-NOTICES.txt", 8 * 1024 * 1024),
        "manifest": ("sync-engine-notices/manifest.json", 4 * 1024 * 1024),
    }
    verified = {}
    for line in lines[1:]:
        fields = line.split(" ")
        if len(fields) != 4 or fields[0] not in expected or fields[0] in verified:
            raise SystemExit("packaged notice index has a malformed row")
        label, digest, size_text, path = fields
        expected_path, maximum = expected[label]
        if path != expected_path or re.fullmatch(r"[0-9a-f]{64}", digest) is None:
            raise SystemExit("packaged notice index has an invalid path or digest")
        if not size_text.isascii() or not size_text.isdecimal() or str(int(size_text)) != size_text:
            raise SystemExit("packaged notice index has an invalid size")
        data = read_bounded(archive, path, maximum)
        if len(data) != int(size_text) or hashlib.sha256(data).hexdigest() != digest:
            raise SystemExit(f"packaged {label} notice evidence differs from its index")
        verified[label] = data
    if set(verified) != set(expected):
        raise SystemExit("packaged notice index is incomplete")
    try:
        manifest = json.loads(verified["manifest"])
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise SystemExit("packaged notice manifest is malformed")
    if manifest.get("schemaVersion") != 1 or manifest.get("status") != "texts-collected-review-required":
        raise SystemExit("packaged notice manifest has an unsupported schema or status")
    combined = verified["combined"]
    declared_combined = manifest.get("combinedNotice")
    if not isinstance(declared_combined, dict) or (
        declared_combined.get("bytes") != len(combined)
        or declared_combined.get("sha256") != hashlib.sha256(combined).hexdigest()
    ):
        raise SystemExit("packaged notice manifest does not bind the readable notice")
    rows = manifest.get("toolchainNotices")
    if not isinstance(rows, list) or len(rows) != 2:
        raise SystemExit("packaged notice manifest lacks the exact NDK notice pair")
    expected_names = {
        "NOTICE": "Android NDK 27.1.12297006 / NOTICE",
        "NOTICE.toolchain": "Android NDK 27.1.12297006 / NOTICE.toolchain",
    }
    seen = set()
    for row in rows:
        if not isinstance(row, dict):
            raise SystemExit("packaged toolchain notice row is malformed")
        name = row.get("sourceName")
        path = row.get("bundlePath")
        digest = row.get("sha256")
        size = row.get("bytes")
        label = row.get("label")
        if (
            name not in expected_names or name in seen or label != expected_names[name]
            or path != f"toolchain/{name}" or not isinstance(size, int)
            or not 0 < size <= 2 * 1024 * 1024
            or not isinstance(digest, str) or re.fullmatch(r"[0-9a-f]{64}", digest) is None
        ):
            raise SystemExit("packaged toolchain notice row is malformed")
        data = read_bounded(archive, "sync-engine-notices/" + path, 2 * 1024 * 1024)
        if len(data) != size or hashlib.sha256(data).hexdigest() != digest:
            raise SystemExit("packaged toolchain notice differs from its manifest")
        readable_section = f"\n===== {label} =====\n".encode() + data
        if readable_section not in combined:
            raise SystemExit("packaged toolchain notice is absent from the readable notice")
        seen.add(name)
    if seen != set(expected_names):
        raise SystemExit("packaged notice manifest lacks the exact NDK notice pair")

with zipfile.ZipFile(package) as archive:
    if len(archive.namelist()) != len(set(archive.namelist())):
        raise SystemExit("package contains duplicate archive member names")
    workers = parse_manifest(
        archive, "syncthing-sha256.txt", worker_min, worker_max)
    guardians = parse_manifest(
        archive, "engine-guardian-sha256.txt", guardian_min, guardian_max)
    verify_helpers(archive, "libsyncthing.so", workers)
    verify_helpers(archive, "libengineguardian.so", guardians)
    verify_notices(archive)
PY

echo "Android native package checks passed for $package."
