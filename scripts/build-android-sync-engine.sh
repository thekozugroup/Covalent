#!/bin/sh
set -eu

test "$#" = 2 || {
  echo "usage: $0 /exact/syncthing/checkout /generated/output" >&2
  exit 2
}
source_dir=${1:?Pass the exact Syncthing source checkout}
output_root=${2:?Pass the generated output directory}
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
guardian_source="$repo_root/packaging/sync-engine/engine-guardian.c"
inventory_tool="$repo_root/scripts/collect-go-target-license-inventory.py"
notice_tool="$repo_root/scripts/collect-sync-engine-notices.py"
link_provenance_tool="$repo_root/scripts/collect-android-native-link-provenance.py"
go_link_wrapper_source="$repo_root/scripts/android-go-link-wrapper.sh"
project_license="$repo_root/LICENSE"
ofl_license="$repo_root/docs/licenses/sync-engine/OFL-1.1.txt"
. "$repo_root/scripts/android-native-budgets.sh"

expected_commit=946e2b83a1f6c6ae119427c09e0a5802940b82ff
expected_go='go version go1.26.7 '
expected_ndk=27.1.12297006
guardian_source_sha=c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579
expected_gomod_sha=a129d6ae9cf20593fab4b1fb04ac09b176c4942d3a4bec9394f9c888fe2d1bd1
expected_gosum_sha=7e9606117eca33e9263181a3d0141e403c940c55022a061d8ed9e22d4bda2acd
source_date_epoch=1785792965
maximum_notice_bytes=$((8 * 1024 * 1024))
maximum_notice_manifest_bytes=$((4 * 1024 * 1024))

test "$(shasum -a 256 "$guardian_source" | awk '{print $1}')" = "$guardian_source_sha" || {
  echo "Reviewed engine guardian source hash does not match" >&2
  exit 1
}

git -C "$source_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
  echo "Syncthing source must be an exact git checkout" >&2
  exit 1
}
actual_commit=$(git -C "$source_dir" rev-parse HEAD)
test "$actual_commit" = "$expected_commit" || {
  echo "Expected Syncthing $expected_commit, found $actual_commit" >&2
  exit 1
}
test -z "$(git -C "$source_dir" status --porcelain=v1 --untracked-files=all)" || {
  echo "Syncthing source checkout must be completely clean" >&2
  exit 1
}
test "$(shasum -a 256 "$source_dir/go.mod" | awk '{print $1}')" = "$expected_gomod_sha" && \
  test "$(shasum -a 256 "$source_dir/go.sum" | awk '{print $1}')" = "$expected_gosum_sha" || {
  echo "Pinned Syncthing module files do not match" >&2
  exit 1
}

test ! -e "$output_root" && test ! -L "$output_root" || {
  echo "Generated Android sync-engine output must be a new path" >&2
  exit 1
}
test -d "$(dirname -- "$output_root")" || {
  echo "Generated Android sync-engine output parent is missing" >&2
  exit 1
}
mkdir "$output_root"
output_root=$(CDPATH='' cd -- "$output_root" && pwd -P)
private_work="$output_root/.private-build"
mkdir "$private_work"
cleanup_private_work() {
  # Go module directories are normally read-only. The Go cleaner repairs their
  # modes before removal, and its explicit cache is confined to this build.
  if command -v go >/dev/null 2>&1 && test -d "$private_work/gomodcache"; then
    GOMODCACHE="$private_work/gomodcache" GOPATH="$private_work/gopath" \
      GOTOOLCHAIN=local go clean -modcache
  fi
  rm -rf "$private_work"
}
trap cleanup_private_work EXIT INT TERM
export GOPATH="$private_work/gopath"
export GOMODCACHE="$private_work/gomodcache"
export GOCACHE="$private_work/gocache"
export GOTMPDIR="$private_work/gotmp"
export TMPDIR="$private_work/gotmp"
export GOFLAGS=-mod=readonly
export GOTOOLCHAIN=local
mkdir "$GOPATH" "$GOMODCACHE" "$GOCACHE" "$GOTMPDIR"

# Build from an archive of the pinned commit, never from the caller's mutable
# checkout. Untracked Go files can otherwise join a package without changing
# HEAD or tracked status. The temporary export also confines generated GUI
# source to this invocation and leaves the supplied checkout byte-for-byte
# untouched.
source_export="$private_work/source-export"
mkdir "$source_export"
git -C "$source_dir" archive --format=tar "$expected_commit" | tar -xf - -C "$source_export"
build_source=$source_export
test -f "$build_source/build.go" && test -f "$build_source/go.mod" || {
  echo "Pinned Syncthing source export is incomplete" >&2
  exit 1
}
test "$(shasum -a 256 "$build_source/go.mod" | awk '{print $1}')" = "$expected_gomod_sha" && \
  test "$(shasum -a 256 "$build_source/go.sum" | awk '{print $1}')" = "$expected_gosum_sha" || {
  echo "Exported Syncthing module files do not match" >&2
  exit 1
}

go_version=$(go version)
case "$go_version" in
  "$expected_go"*) ;;
  *) echo "Expected Go 1.26.7, found: $go_version" >&2; exit 1 ;;
esac

ndk_dir=${COVALENT_ANDROID_NDK_HOME:-${ANDROID_NDK_HOME:-}}
test -n "$ndk_dir" || {
  echo "Set COVALENT_ANDROID_NDK_HOME to Android NDK $expected_ndk" >&2
  exit 1
}
ndk_dir=$(CDPATH='' cd -- "$ndk_dir" && pwd -P)
test -f "$ndk_dir/source.properties" && test ! -L "$ndk_dir/source.properties" || {
  echo "NDK source.properties is missing under $ndk_dir" >&2
  exit 1
}
actual_ndk=$(awk -F= '
  $1 ~ /^Pkg\.Revision[[:space:]]*$/ {
    value = $2
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    print value
    exit
  }
' "$ndk_dir/source.properties")
test "$actual_ndk" = "$expected_ndk" || {
  echo "Expected Android NDK $expected_ndk" >&2
  exit 1
}
for notice_name in NOTICE NOTICE.toolchain; do
  notice_path="$ndk_dir/$notice_name"
  test -f "$notice_path" && test ! -L "$notice_path" || {
    echo "NDK $notice_name must be a regular non-symbolic-link file" >&2
    exit 1
  }
  notice_size=$(wc -c < "$notice_path" | tr -d '[:space:]')
  test "$notice_size" -gt 0 && test "$notice_size" -le 2097152 || {
    echo "NDK $notice_name is empty or exceeds 2 MiB" >&2
    exit 1
  }
done
ndk_notice_sha=$(shasum -a 256 "$ndk_dir/NOTICE" | awk '{print $1}')
ndk_toolchain_notice_sha=$(shasum -a 256 "$ndk_dir/NOTICE.toolchain" | awk '{print $1}')

prebuilt_root="$ndk_dir/toolchains/llvm/prebuilt"
toolchain=
for candidate in "$prebuilt_root"/*; do
  test -d "$candidate/bin" || continue
  test -z "$toolchain" || {
    echo "More than one NDK host toolchain exists under $prebuilt_root" >&2
    exit 1
  }
  toolchain=$candidate
done
test -n "$toolchain" || {
  echo "No NDK LLVM host toolchain found under $prebuilt_root" >&2
  exit 1
}

jni_root="$output_root/jniLibs"
assets_root="$output_root/assets"
reports_root="$output_root/reports"
mkdir -p "$jni_root/arm64-v8a" "$jni_root/x86_64" "$assets_root" "$reports_root"
hash_file="$assets_root/syncthing-sha256.txt"
guardian_hash_file="$assets_root/engine-guardian-sha256.txt"
: > "$hash_file"
: > "$guardian_hash_file"

fingerprint_source_export() {
  phase=$1
  expected=$2
  destination=$3
  python3 - "$build_source" "$phase" "$expected" "$destination" <<'PY'
import hashlib
import json
import os
import pathlib
import stat
import struct
import sys

root = pathlib.Path(sys.argv[1])
phase, expected_path, destination = sys.argv[2:]
generated = {
    "lib/api/auto/gui.files.go",
    "cmd/infra/strelaypoolsrv/auto/gui.files.go",
}
maximum_files = 100_000
maximum_bytes = 1024 * 1024 * 1024
maximum_generated_bytes = 24 * 1024 * 1024
digest = hashlib.sha256(b"covalent-android-sync-engine-source-v1\0")
files = total = 0
generated_rows = []
for base, directories, names in os.walk(root, followlinks=False):
    directories.sort()
    names.sort()
    for name in directories:
        if not stat.S_ISDIR((pathlib.Path(base) / name).lstat().st_mode):
            raise SystemExit("source export contains an unsafe directory entry")
    for name in names:
        path = pathlib.Path(base) / name
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode):
            raise SystemExit("source export contains a non-regular file")
        relative = path.relative_to(root).as_posix()
        files += 1
        total += metadata.st_size
        if files > maximum_files or total > maximum_bytes:
            raise SystemExit("source export exceeds its bound")
        content = hashlib.sha256()
        with path.open("rb") as source:
            while True:
                chunk = source.read(1024 * 1024)
                if not chunk:
                    break
                content.update(chunk)
        if relative in generated:
            generated_rows.append({
                "path": relative,
                "bytes": metadata.st_size,
                "sha256": content.hexdigest(),
            })
            continue
        encoded = relative.encode("utf-8")
        digest.update(struct.pack(">I", len(encoded)))
        digest.update(encoded)
        digest.update(struct.pack(">IQ", stat.S_IMODE(metadata.st_mode), metadata.st_size))
        digest.update(content.digest())
generated_rows.sort(key=lambda row: row["path"])
if phase == "before-assets":
    if generated_rows:
        raise SystemExit("source export unexpectedly contains generated assets")
elif phase == "after-collection":
    if {row["path"] for row in generated_rows} != generated:
        raise SystemExit("official asset generation did not produce the exact expected files")
    if sum(row["bytes"] for row in generated_rows) > maximum_generated_bytes:
        raise SystemExit("generated assets exceed their bound")
else:
    raise SystemExit("unknown source fingerprint phase")
value = {
    "schemaVersion": 1,
    "phase": phase,
    "sourceTreeSha256ExcludingGeneratedAssets": digest.hexdigest(),
    "sourceFileCountIncludingGeneratedAssets": files,
    "sourceBytesIncludingGeneratedAssets": total,
    "generatedAssets": generated_rows,
}
if expected_path != "-":
    expected_value = json.loads(pathlib.Path(expected_path).read_text())
    expected_files = expected_value["sourceFileCountIncludingGeneratedAssets"]
    expected_bytes = expected_value["sourceBytesIncludingGeneratedAssets"]
    if expected_value["generatedAssets"]:
        expected_files -= len(expected_value["generatedAssets"])
        expected_bytes -= sum(row["bytes"] for row in expected_value["generatedAssets"])
    if (
        value["sourceTreeSha256ExcludingGeneratedAssets"] !=
            expected_value["sourceTreeSha256ExcludingGeneratedAssets"]
        or files - len(generated_rows) != expected_files
        or total - sum(row["bytes"] for row in generated_rows) != expected_bytes
    ):
        raise SystemExit("non-generated source bytes changed")
pathlib.Path(destination).write_text(json.dumps(value, sort_keys=True, indent=2) + "\n")
PY
}

fingerprint_source_export before-assets - "$reports_root/source-before-assets.json"
(
  cd "$build_source"
  env \
    BUILD_HOST=covalent-android-release \
    BUILD_USER=covalent-android-release \
    GOFLAGS=-mod=readonly \
    GOTOOLCHAIN=local \
    SOURCE_DATE_EPOCH="$source_date_epoch" \
    go run build.go assets
)

build_one() {
  abi=$1
  goarch=$2
  compiler=$3
  output="$jni_root/$abi/libsyncthing.so"
  cc="$toolchain/bin/$compiler"
  link_map="$reports_root/syncthing-link-$abi.map"
  driver_trace="$reports_root/syncthing-driver-$abi.txt"
  dynamic_report="$reports_root/syncthing-dynamic-$abi.txt"
  provenance_report="$reports_root/syncthing-link-provenance-$abi.json"
  link_marker="$reports_root/syncthing-link-$abi.invoked"
  link_wrapper="$private_work/syncthing-link-$abi.sh"
  test -x "$cc" || { echo "Missing NDK compiler: $cc" >&2; exit 1; }
  case "$link_wrapper" in
    *[[:space:]]*) echo "Android external linker wrapper path contains whitespace" >&2; exit 1 ;;
  esac
  cp "$go_link_wrapper_source" "$link_wrapper"
  chmod 700 "$link_wrapper"

  (
    cd "$build_source"
    env \
      BUILD_HOST=covalent-android-release \
      BUILD_USER=covalent-android-release \
      CGO_ENABLED=1 \
      GOFLAGS=-mod=readonly \
      GOTOOLCHAIN=local \
      SOURCE_DATE_EPOCH="$source_date_epoch" \
      COVALENT_REAL_CLANG="$cc" \
      COVALENT_LINK_CLASSIFIER="$link_provenance_tool" \
      COVALENT_LINK_PRIVATE_ROOT="$private_work" \
      COVALENT_LINK_MAP="$link_map" \
      COVALENT_DRIVER_TRACE="$driver_trace" \
      COVALENT_LINK_MARKER="$link_marker" \
      EXTRA_LDFLAGS="-checklinkname=0 -linkmode=external -extld=$link_wrapper -extldflags=-Wl,-z,max-page-size=16384" \
      go run build.go \
        -goos android \
        -goarch "$goarch" \
        -cc "$cc" \
        -no-upgrade \
        -version v2.1.3 \
        -build-out "$output" \
        build syncthing
  )
  test -s "$link_map" && test "$(wc -c < "$link_map" | tr -d '[:space:]')" -le 134217728 || {
    echo "$abi Syncthing final link map is empty or exceeds 128 MiB" >&2
    exit 1
  }

  size=$(wc -c < "$output" | tr -d '[:space:]')
  test "$size" -ge "$COVALENT_SYNC_ENGINE_MIN_BYTES" && \
    test "$size" -le "$COVALENT_SYNC_ENGINE_MAX_BYTES" || {
    echo "$abi helper size $size is outside [$COVALENT_SYNC_ENGINE_MIN_BYTES, $COVALENT_SYNC_ENGINE_MAX_BYTES]" >&2
    exit 1
  }

  readelf="$toolchain/bin/llvm-readelf"
  "$readelf" -h "$output" | grep -Eq 'Type:[[:space:]]+DYN' || {
    echo "$abi helper is not a position-independent ET_DYN ELF" >&2
    exit 1
  }
  python3 - "$readelf" "$output" <<'PY' || {
import subprocess
import sys

report = subprocess.run(
    [sys.argv[1], "-lW", sys.argv[2]],
    check=True,
    capture_output=True,
    text=True,
).stdout
alignments = [int(line.split()[-1], 0) for line in report.splitlines() if line.split()[:1] == ["LOAD"]]
if not alignments or min(alignments) < 16384:
    raise SystemExit(1)
PY
    echo "$abi helper has a PT_LOAD segment aligned below 16 KiB" >&2
    exit 1
  }
  "$readelf" -dW "$output" > "$dynamic_report"
  test "$(wc -c < "$dynamic_report" | tr -d '[:space:]')" -le 1048576 || {
    echo "$abi Syncthing dynamic report exceeds 1 MiB" >&2
    exit 1
  }
  grep -Eq 'TEXTREL|DT_TEXTREL' "$dynamic_report" && {
    echo "$abi helper contains a text relocation" >&2
    exit 1
  }

  needed=$(sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' "$dynamic_report" | sort -u)
  unexpected=$(printf '%s\n' "$needed" | sed '/^$/d' | grep -Ev '^(libc|libdl|liblog|libm)\.so$' || true)
  test -z "$unexpected" || {
    echo "$abi helper has unexpected runtime dependencies: $unexpected" >&2
    exit 1
  }
  python3 "$link_provenance_tool" \
    --component syncthing \
    --abi "$abi" \
    --ndk-root "$ndk_dir" \
    --expected-revision "$expected_ndk" \
    --link-map "$link_map" \
    --driver-trace "$driver_trace" \
    --dynamic-report "$dynamic_report" \
    --binary "$output" \
    --output "$provenance_report"
  test -d "$link_marker" && test ! -L "$link_marker" || {
    echo "$abi Syncthing linker invocation marker is missing or unsafe" >&2
    exit 1
  }
  rmdir "$link_marker"

  hash=$(shasum -a 256 "$output" | awk '{print $1}')
  printf '%s %s %s\n' "$abi" "$hash" "$size" >> "$hash_file"
  echo "  $abi sync engine: $size bytes, sha256=$hash"
}

build_guardian() {
  abi=$1
  compiler=$2
  output="$jni_root/$abi/libengineguardian.so"
  cc="$toolchain/bin/$compiler"
  readelf="$toolchain/bin/llvm-readelf"
  dynamic_report="$reports_root/guardian-dynamic-$abi.txt"
  symbol_report="$reports_root/guardian-symbols-$abi.txt"
  link_map="$reports_root/guardian-link-$abi.map"
  driver_trace="$reports_root/guardian-driver-$abi.txt"
  provenance_report="$reports_root/guardian-link-provenance-$abi.json"
  test -x "$cc" || { echo "Missing NDK compiler: $cc" >&2; exit 1; }

  "$cc" -### \
    -std=c11 -Wall -Wextra -Werror -O2 \
    -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fPIE -pie \
    -Wl,--as-needed,-z,relro,-z,now,-z,noexecstack,-z,max-page-size=16384,-Map,"$link_map" \
    "$guardian_source" -o "$output" >/dev/null 2> "$driver_trace"
  test -s "$driver_trace" && test "$(wc -c < "$driver_trace" | tr -d '[:space:]')" -le 4194304 || {
    echo "$abi guardian Clang driver trace is empty or exceeds 4 MiB" >&2
    exit 1
  }
  "$cc" \
    -std=c11 -Wall -Wextra -Werror -O2 \
    -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fPIE -pie \
    -Wl,--as-needed,-z,relro,-z,now,-z,noexecstack,-z,max-page-size=16384,-Map,"$link_map" \
    "$guardian_source" -o "$output"
  test -s "$link_map" && test "$(wc -c < "$link_map" | tr -d '[:space:]')" -le 134217728 || {
    echo "$abi guardian final link map is empty or exceeds 128 MiB" >&2
    exit 1
  }

  size=$(wc -c < "$output" | tr -d '[:space:]')
  test "$size" -ge "$COVALENT_ENGINE_GUARDIAN_MIN_BYTES" && \
    test "$size" -le "$COVALENT_ENGINE_GUARDIAN_MAX_BYTES" || {
    echo "$abi guardian size $size is outside [$COVALENT_ENGINE_GUARDIAN_MIN_BYTES, $COVALENT_ENGINE_GUARDIAN_MAX_BYTES]" >&2
    exit 1
  }
  "$readelf" -dW "$output" > "$dynamic_report"
  "$readelf" -Ws "$output" > "$symbol_report"
  test "$(wc -c < "$dynamic_report" | tr -d '[:space:]')" -le 131072 || {
    echo "$abi guardian dynamic report exceeded 128 KiB" >&2
    exit 1
  }
  test "$(wc -c < "$symbol_report" | tr -d '[:space:]')" -le 131072 || {
    echo "$abi guardian symbol report exceeded 128 KiB" >&2
    exit 1
  }
  needed=$(sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' "$dynamic_report" | sort -u)
  printf '  %s guardian dynamic dependencies: %s\n' "$abi" "${needed:-none}"
  "$readelf" -h "$output" | grep -Eq 'Type:[[:space:]]+DYN' || {
    echo "$abi guardian is not a position-independent ET_DYN ELF" >&2
    exit 1
  }
  python3 - "$readelf" "$output" <<'PY' || {
import subprocess
import sys

report = subprocess.run(
    [sys.argv[1], "-lW", sys.argv[2]],
    check=True,
    capture_output=True,
    text=True,
).stdout
lines = report.splitlines()
alignments = [int(line.split()[-1], 0) for line in lines if line.split()[:1] == ["LOAD"]]
if not alignments or min(alignments) < 16384:
    raise SystemExit(1)
if not any("GNU_RELRO" in line for line in lines):
    raise SystemExit(1)
stack = [line for line in lines if "GNU_STACK" in line]
if len(stack) != 1 or "RWE" in stack[0].split():
    raise SystemExit(1)
PY
    echo "$abi guardian failed its PT_LOAD/RELRO/non-executable-stack audit" >&2
    exit 1
  }
  grep -Eq 'BIND_NOW|FLAGS.*NOW' "$dynamic_report" || {
    echo "$abi guardian is missing immediate relocation binding" >&2
    exit 1
  }
  grep -Eq 'TEXTREL|DT_TEXTREL' "$dynamic_report" && {
    echo "$abi guardian contains a text relocation" >&2
    exit 1
  }
  unexpected=$(printf '%s\n' "$needed" | sed '/^$/d' | grep -Ev '^libc\.so$' || true)
  test -z "$unexpected" || {
    echo "$abi guardian has unexpected runtime dependencies: $unexpected" >&2
    exit 1
  }
  python3 "$link_provenance_tool" \
    --component guardian \
    --abi "$abi" \
    --ndk-root "$ndk_dir" \
    --expected-revision "$expected_ndk" \
    --link-map "$link_map" \
    --driver-trace "$driver_trace" \
    --dynamic-report "$dynamic_report" \
    --binary "$output" \
    --output "$provenance_report"

  hash=$(shasum -a 256 "$output" | awk '{print $1}')
  printf '%s %s %s\n' "$abi" "$hash" "$size" >> "$guardian_hash_file"
  echo "  $abi guardian: $size bytes, sha256=$hash"
}

build_one arm64-v8a arm64 aarch64-linux-android26-clang
build_one x86_64 amd64 x86_64-linux-android26-clang
build_guardian arm64-v8a aarch64-linux-android26-clang
build_guardian x86_64 x86_64-linux-android26-clang

test "$(wc -l < "$hash_file" | tr -d '[:space:]')" = 2
test "$(wc -l < "$guardian_hash_file" | tr -d '[:space:]')" = 2

collect_target_graph() {
  abi=$1
  goarch=$2
  compiler=$3
  report="$reports_root/go-target-deps-$abi.ndjson"
  cc="$toolchain/bin/$compiler"
  (
    cd "$build_source"
    env \
      GOOS=android \
      GOARCH="$goarch" \
      CGO_ENABLED=1 \
      CC="$cc" \
      GOFLAGS='-mod=readonly -tags=noupgrade' \
      GOTOOLCHAIN=local \
      go list -mod=readonly -tags=noupgrade -deps -json ./cmd/syncthing > "$report"
  )
  test -s "$report" && test "$(wc -c < "$report" | tr -d '[:space:]')" -le 134217728 || {
    echo "$abi target dependency graph is empty or exceeds 128 MiB" >&2
    exit 1
  }
}

collect_target_graph arm64-v8a arm64 aarch64-linux-android26-clang
collect_target_graph x86_64 amd64 x86_64-linux-android26-clang
(
  cd "$build_source"
  go mod verify
)

inventory="$reports_root/android-target-license-inventory.json"
python3 "$inventory_tool" \
  --source-root "$build_source" \
  --module-cache "$GOMODCACHE" \
  --target arm64-v8a android arm64 1 noupgrade "$reports_root/go-target-deps-arm64-v8a.ndjson" \
  --target x86_64 android amd64 1 noupgrade "$reports_root/go-target-deps-x86_64.ndjson" \
  --output "$inventory"

go_root=$(go env GOROOT)
go_root=$(CDPATH='' cd -- "$go_root" && pwd -P)
notices="$assets_root/sync-engine-notices"
# AGP expands .gz assets and removes that suffix. .tgz keeps the manifest name and gzip bytes.
python3 "$notice_tool" \
  --inventory "$inventory" \
  --source-root "$build_source" \
  --module-cache "$GOMODCACHE" \
  --go-root "$go_root" \
  --guardian-source "$guardian_source" \
  --project-license "$project_license" \
  --ofl-license "$ofl_license" \
  --toolchain-notice "Android NDK $expected_ndk / NOTICE" NOTICE "$ndk_dir/NOTICE" "$ndk_notice_sha" \
  --toolchain-notice "Android NDK $expected_ndk / NOTICE.toolchain" NOTICE.toolchain "$ndk_dir/NOTICE.toolchain" "$ndk_toolchain_notice_sha" \
  --source-archive-suffix .tgz \
  --output "$notices"

python3 - "$reports_root" "$notices/manifest.json" "$hash_file" "$guardian_hash_file" <<'PY'
import hashlib
import json
import pathlib
import sys

reports = pathlib.Path(sys.argv[1])
notice_manifest = json.loads(pathlib.Path(sys.argv[2]).read_text())
binary_manifests = {}
for component, manifest_path in (
    ("syncthing", pathlib.Path(sys.argv[3])),
    ("guardian", pathlib.Path(sys.argv[4])),
):
    parsed = {}
    for line in manifest_path.read_text().splitlines():
        fields = line.split(" ")
        if len(fields) != 3:
            raise SystemExit("native binary hash manifest is malformed")
        abi, digest, size_text = fields
        parsed[abi] = (int(size_text), digest)
    if set(parsed) != {"arm64-v8a", "x86_64"}:
        raise SystemExit("native binary hash manifest is incomplete")
    binary_manifests[component] = parsed
expected_notices = {
    row["sourceName"]: (row["bytes"], row["sha256"])
    for row in notice_manifest.get("toolchainNotices", [])
}
if set(expected_notices) != {"NOTICE", "NOTICE.toolchain"}:
    raise SystemExit("Android notice manifest lacks the exact NDK notice pair")
rows = []
for component in ("guardian", "syncthing"):
    for abi in ("arm64-v8a", "x86_64"):
        path = reports / f"{component}-link-provenance-{abi}.json"
        raw = path.read_bytes()
        value = json.loads(raw)
        observed = {
            name: (record["bytes"], record["sha256"])
            for name, record in value["ndk"]["files"].items()
            if name in {"NOTICE", "NOTICE.toolchain"}
        }
        if observed != expected_notices:
            raise SystemExit("NDK link and notice evidence disagree")
        binary = value["evidence"]["binary"]
        if (binary["bytes"], binary["sha256"]) != binary_manifests[component][abi]:
            raise SystemExit("native binary and link provenance evidence disagree")
        rows.append({
            "abi": abi,
            "component": component,
            "manifestBytes": len(raw),
            "manifestSha256": hashlib.sha256(raw).hexdigest(),
        })
index = {
    "schemaVersion": 1,
    "status": "link-inputs-classified-review-required",
    "records": rows,
}
(reports / "android-sync-engine-link-provenance-index.json").write_text(
    json.dumps(index, indent=2, sort_keys=True) + "\n"
)
PY

combined="$notices/THIRD-PARTY-NOTICES.txt"
notice_manifest="$notices/manifest.json"
combined_size=$(wc -c < "$combined" | tr -d '[:space:]')
manifest_size=$(wc -c < "$notice_manifest" | tr -d '[:space:]')
test "$combined_size" -gt 0 && test "$combined_size" -le "$maximum_notice_bytes" || {
  echo "Combined Android notices are empty or exceed the 8 MiB viewer bound" >&2
  exit 1
}
test "$manifest_size" -gt 0 && test "$manifest_size" -le "$maximum_notice_manifest_bytes" || {
  echo "Android notice manifest is empty or exceeds the 4 MiB viewer bound" >&2
  exit 1
}
combined_sha=$(shasum -a 256 "$combined" | awk '{print $1}')
manifest_sha=$(shasum -a 256 "$notice_manifest" | awk '{print $1}')
cat > "$assets_root/sync-engine-notices-index.txt" <<EOF
1
combined $combined_sha $combined_size sync-engine-notices/THIRD-PARTY-NOTICES.txt
manifest $manifest_sha $manifest_size sync-engine-notices/manifest.json
EOF

fingerprint_source_export after-collection "$reports_root/source-before-assets.json" \
  "$reports_root/source-after-collection.json"
test "$(shasum -a 256 "$build_source/go.mod" | awk '{print $1}')" = "$expected_gomod_sha" && \
  test "$(shasum -a 256 "$build_source/go.sum" | awk '{print $1}')" = "$expected_gosum_sha" || {
  echo "Syncthing module files changed during the Android build" >&2
  exit 1
}
test "$(git -C "$source_dir" rev-parse HEAD)" = "$expected_commit" && \
  test -z "$(git -C "$source_dir" status --porcelain=v1 --untracked-files=all)" || {
  echo "Pinned Syncthing checkout changed during the Android build" >&2
  exit 1
}
echo "  Android target notices: $combined_size bytes, sha256=$combined_sha"
