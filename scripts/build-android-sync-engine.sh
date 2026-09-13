#!/bin/sh
set -eu

test "$#" = 2 || { echo "usage: $0 /repo/packaging/rclone /generated/output" >&2; exit 2; }
source_dir=${1:?Pass the repository rclone module}
output_root=${2:?Pass the generated output directory}
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
expected_source="$repo_root/packaging/rclone"
guardian_source="$repo_root/packaging/sync-engine/engine-guardian.c"
inventory_tool="$repo_root/scripts/collect-go-target-license-inventory.py"
notice_tool="$repo_root/scripts/collect-sync-engine-notices.py"
link_provenance_tool="$repo_root/scripts/collect-android-native-link-provenance.py"
go_link_wrapper_source="$repo_root/scripts/android-go-link-wrapper.sh"
project_license="$repo_root/LICENSE"
. "$repo_root/scripts/android-native-budgets.sh"

engine_version=v1.75.1
engine_commit=687d264b689b8c49a67e2e52a8a5e0caa01c04ce
expected_go='go version go1.26.7 '
expected_ndk=27.1.12297006
guardian_source_sha=c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579
expected_gomod_sha=1708132fb012d15c89e7863a79abea50c66895e78d8b85037275db213c1d103f
expected_gosum_sha=801fcfc81dd2f84417d5410c04aa4fd5936387246372eaf8920c2b0492ffa89b
rclone_license_sha=9266eae9c6a441de0f6847f19ac8f09b280d55612b079eca03b3d49b822c1a71
source_date_epoch=1785792965
maximum_notice_bytes=$((8 * 1024 * 1024))
maximum_notice_manifest_bytes=$((4 * 1024 * 1024))

sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
size_of() { wc -c < "$1" | tr -d '[:space:]'; }
fail() { echo "$1" >&2; exit 1; }

test "$(sha256 "$guardian_source")" = "$guardian_source_sha" || fail "Reviewed engine guardian source hash does not match"
case "$source_dir:$output_root" in /*:/*) ;; *) fail "source and output paths must be absolute" ;; esac
source_dir=$(CDPATH='' cd -- "$source_dir" && pwd -P)
test "$source_dir" = "$expected_source" || fail "worker source must be the repository packaging/rclone module"
test "$(sha256 "$source_dir/go.mod")" = "$expected_gomod_sha" && \
  test "$(sha256 "$source_dir/go.sum")" = "$expected_gosum_sha" && \
  test "$(sha256 "$source_dir/LICENSE.rclone")" = "$rclone_license_sha" ||
  fail "restricted rclone module differs"
source_commit=$(git -C "$repo_root" rev-parse HEAD)
source_tree=$(git -C "$repo_root" rev-parse HEAD:packaging/rclone)

test ! -e "$output_root" && test ! -L "$output_root" || fail "Generated Android sync-engine output must be a new path"
test -d "$(dirname -- "$output_root")" || fail "Generated Android sync-engine output parent is missing"
mkdir "$output_root"
output_root=$(CDPATH='' cd -- "$output_root" && pwd -P)
private_work="$output_root/.private-build"
mkdir "$private_work"
cleanup_private_work() {
  if command -v go >/dev/null 2>&1 && test -d "$private_work/gomodcache"; then
    GOMODCACHE="$private_work/gomodcache" GOPATH="$private_work/gopath" GOTOOLCHAIN=local go clean -modcache >/dev/null 2>&1 || true
  fi
  rm -rf "$private_work"
}
trap cleanup_private_work EXIT INT TERM
export GOPATH="$private_work/gopath"
export GOMODCACHE="$private_work/gomodcache"
export GOCACHE="$private_work/gocache"
export GOTMPDIR="$private_work/gotmp"
export TMPDIR="$private_work/gotmp"
export GOFLAGS='-buildvcs=false -mod=readonly -trimpath'
export GOTOOLCHAIN=local
mkdir "$GOPATH" "$GOMODCACHE" "$GOCACHE" "$GOTMPDIR"

build_source="$private_work/source-export"
mkdir "$build_source"
source_archive="$private_work/source-export.tar"
git -C "$repo_root" archive --format=tar HEAD:packaging/rclone > "$source_archive"
tar -xf "$source_archive" -C "$build_source"
test "$(sha256 "$build_source/go.mod")" = "$expected_gomod_sha" && \
  test "$(sha256 "$build_source/go.sum")" = "$expected_gosum_sha" && \
  test "$(sha256 "$build_source/LICENSE.rclone")" = "$rclone_license_sha" ||
  fail "exported restricted rclone module differs"

go_version=$(go version)
case "$go_version" in "$expected_go"*) ;; *) echo "Expected Go 1.26.7, found: $go_version" >&2; exit 1 ;; esac
ndk_dir=${COVALENT_ANDROID_NDK_HOME:-${ANDROID_NDK_HOME:-}}
test -n "$ndk_dir" || fail "Set COVALENT_ANDROID_NDK_HOME to Android NDK $expected_ndk"
ndk_dir=$(CDPATH='' cd -- "$ndk_dir" && pwd -P)
test -f "$ndk_dir/source.properties" && test ! -L "$ndk_dir/source.properties" || fail "NDK source.properties is missing"
actual_ndk=$(awk -F= '$1 ~ /^Pkg\.Revision[[:space:]]*$/ { value=$2; gsub(/^[[:space:]]+|[[:space:]]+$/, "", value); print value; exit }' "$ndk_dir/source.properties")
test "$actual_ndk" = "$expected_ndk" || fail "Expected Android NDK $expected_ndk"
for notice_name in NOTICE NOTICE.toolchain; do
  notice_path="$ndk_dir/$notice_name"
  test -f "$notice_path" && test ! -L "$notice_path" || fail "NDK $notice_name must be a regular file"
  notice_size=$(size_of "$notice_path")
  test "$notice_size" -gt 0 && test "$notice_size" -le 2097152 || fail "NDK $notice_name is empty or exceeds 2 MiB"
done
ndk_notice_sha=$(sha256 "$ndk_dir/NOTICE")
ndk_toolchain_notice_sha=$(sha256 "$ndk_dir/NOTICE.toolchain")
prebuilt_root="$ndk_dir/toolchains/llvm/prebuilt"
toolchain=
for candidate in "$prebuilt_root"/*; do
  test -d "$candidate/bin" || continue
  test -z "$toolchain" || fail "More than one NDK host toolchain exists"
  toolchain=$candidate
done
test -n "$toolchain" || fail "No NDK LLVM host toolchain found"

jni_root="$output_root/jniLibs"
assets_root="$output_root/assets"
reports_root="$output_root/reports"
mkdir -p "$jni_root/arm64-v8a" "$jni_root/x86_64" "$assets_root" "$reports_root"
hash_file="$assets_root/rclone-sha256.txt"
guardian_hash_file="$assets_root/engine-guardian-sha256.txt"
: > "$hash_file"
: > "$guardian_hash_file"

verify_elf() {
  elf_abi=$1
  elf_readelf=$2
  elf_output=$3
  elf_dynamic_report=$4
  "$elf_readelf" -h "$elf_output" | grep -Eq 'Type:[[:space:]]+DYN' || fail "$elf_abi helper is not a position-independent ET_DYN ELF"
  python3 - "$elf_readelf" "$elf_output" <<'PY' || exit 1
import subprocess, sys
report = subprocess.run([sys.argv[1], "-lW", sys.argv[2]], check=True, capture_output=True, text=True).stdout
alignments = [int(line.split()[-1], 0) for line in report.splitlines() if line.split()[:1] == ["LOAD"]]
if not alignments or min(alignments) < 16384:
    raise SystemExit("PT_LOAD alignment below 16 KiB")
PY
  "$elf_readelf" -dW "$elf_output" > "$elf_dynamic_report"
  test "$(size_of "$elf_dynamic_report")" -le 1048576 || fail "$elf_abi dynamic report exceeds 1 MiB"
  ! grep -Eq 'TEXTREL|DT_TEXTREL' "$elf_dynamic_report" || fail "$elf_abi helper contains a text relocation"
}

build_worker() {
  abi=$1
  goarch=$2
  compiler=$3
  output="$jni_root/$abi/libcovalentrclone.so"
  cc="$toolchain/bin/$compiler"
  readelf="$toolchain/bin/llvm-readelf"
  link_wrapper="$private_work/android-go-link-$abi.sh"
  link_map="$reports_root/rclone-link-$abi.map"
  driver_trace="$reports_root/rclone-driver-$abi.txt"
  link_marker="$private_work/rclone-link-marker-$abi"
  dynamic_report="$reports_root/rclone-dynamic-$abi.txt"
  provenance_report="$reports_root/rclone-link-provenance-$abi.json"
  install -m 0700 "$go_link_wrapper_source" "$link_wrapper"
  test -x "$cc" || fail "Missing NDK compiler: $cc"
  (
    cd "$build_source"
    env GOOS=android GOARCH="$goarch" CGO_ENABLED=1 CC="$cc" SOURCE_DATE_EPOCH="$source_date_epoch" \
      COVALENT_REAL_CLANG="$cc" COVALENT_LINK_CLASSIFIER="$link_provenance_tool" \
      COVALENT_LINK_PRIVATE_ROOT="$private_work" COVALENT_LINK_MAP="$link_map" \
      COVALENT_DRIVER_TRACE="$driver_trace" COVALENT_LINK_MARKER="$link_marker" \
      go build -buildmode=pie \
        -ldflags "-s -w -linkmode=external -extld=$link_wrapper -extldflags=-Wl,-z,max-page-size=16384 -X github.com/rclone/rclone/fs.Version=$engine_version" \
        -o "$output" .
  )
  test -s "$link_map" && test "$(size_of "$link_map")" -le 134217728 || fail "$abi rclone final link map is invalid"
  size=$(size_of "$output")
  test "$size" -ge "$COVALENT_SYNC_ENGINE_MIN_BYTES" && test "$size" -le "$COVALENT_SYNC_ENGINE_MAX_BYTES" || fail "$abi helper size is outside its reviewed bound"
  verify_elf "$abi" "$readelf" "$output" "$dynamic_report"
  needed=$(sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' "$dynamic_report" | sort -u)
  unexpected=$(printf '%s\n' "$needed" | sed '/^$/d' | grep -Ev '^(libc|libdl|liblog|libm)\.so$' || true)
  test -z "$unexpected" || fail "$abi helper has unexpected runtime dependencies: $unexpected"
  python3 "$link_provenance_tool" --component rclone --abi "$abi" --ndk-root "$ndk_dir" \
    --expected-revision "$expected_ndk" --link-map "$link_map" --driver-trace "$driver_trace" \
    --dynamic-report "$dynamic_report" --binary "$output" --output "$provenance_report"
  test -d "$link_marker" && test ! -L "$link_marker" || fail "$abi rclone linker invocation marker is missing"
  rmdir "$link_marker"
  hash=$(sha256 "$output")
  printf '%s %s %s\n' "$abi" "$hash" "$size" >> "$hash_file"
  echo "  $abi rclone: $size bytes, sha256=$hash"
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
  "$cc" -### -std=c11 -Wall -Wextra -Werror -O2 -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fPIE -pie \
    -Wl,--as-needed,-z,relro,-z,now,-z,noexecstack,-z,max-page-size=16384,-Map,"$link_map" "$guardian_source" -o "$output" >/dev/null 2> "$driver_trace"
  "$cc" -std=c11 -Wall -Wextra -Werror -O2 -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fPIE -pie \
    -Wl,--as-needed,-z,relro,-z,now,-z,noexecstack,-z,max-page-size=16384,-Map,"$link_map" "$guardian_source" -o "$output"
  size=$(size_of "$output")
  test "$size" -ge "$COVALENT_ENGINE_GUARDIAN_MIN_BYTES" && test "$size" -le "$COVALENT_ENGINE_GUARDIAN_MAX_BYTES" || fail "$abi guardian size is outside its reviewed bound"
  verify_elf "$abi guardian" "$readelf" "$output" "$dynamic_report"
  "$readelf" -Ws "$output" > "$symbol_report"
  grep -Eq 'BIND_NOW|FLAGS.*NOW' "$dynamic_report" || fail "$abi guardian is missing immediate relocation binding"
  python3 "$link_provenance_tool" --component guardian --abi "$abi" --ndk-root "$ndk_dir" \
    --expected-revision "$expected_ndk" --link-map "$link_map" --driver-trace "$driver_trace" \
    --dynamic-report "$dynamic_report" --binary "$output" --output "$provenance_report"
  hash=$(sha256 "$output")
  printf '%s %s %s\n' "$abi" "$hash" "$size" >> "$guardian_hash_file"
  echo "  $abi guardian: $size bytes, sha256=$hash"
}

build_worker arm64-v8a arm64 aarch64-linux-android26-clang
build_worker x86_64 amd64 x86_64-linux-android26-clang
build_guardian arm64-v8a aarch64-linux-android26-clang
build_guardian x86_64 x86_64-linux-android26-clang

collect_target_graph() {
  abi=$1; goarch=$2; compiler=$3
  report="$reports_root/go-target-deps-$abi.ndjson"
  (cd "$build_source" && env GOOS=android GOARCH="$goarch" CGO_ENABLED=1 CC="$toolchain/bin/$compiler" \
    go list -deps -json . > "$report")
  test -s "$report" && test "$(size_of "$report")" -le 134217728 || fail "$abi target dependency graph is invalid"
}
collect_target_graph arm64-v8a arm64 aarch64-linux-android26-clang
collect_target_graph x86_64 amd64 x86_64-linux-android26-clang
(cd "$build_source" && go mod verify)

inventory="$reports_root/android-target-license-inventory.json"
python3 "$inventory_tool" --source-root "$build_source" --module-cache "$GOMODCACHE" \
  --target arm64-v8a android arm64 1 - "$reports_root/go-target-deps-arm64-v8a.ndjson" \
  --target x86_64 android amd64 1 - "$reports_root/go-target-deps-x86_64.ndjson" --output "$inventory"
go_root=$(go env GOROOT)
go_root=$(CDPATH='' cd -- "$go_root" && pwd -P)
notices="$assets_root/sync-engine-notices"
python3 "$notice_tool" --inventory "$inventory" --source-root "$build_source" --module-cache "$GOMODCACHE" \
  --go-root "$go_root" --guardian-source "$guardian_source" --project-license "$project_license" \
  --toolchain-notice "Android NDK $expected_ndk / NOTICE" NOTICE "$ndk_dir/NOTICE" "$ndk_notice_sha" \
  --toolchain-notice "Android NDK $expected_ndk / NOTICE.toolchain" NOTICE.toolchain "$ndk_dir/NOTICE.toolchain" "$ndk_toolchain_notice_sha" \
  --source-archive-suffix .tgz --output "$notices"

# The Settings notice viewer renders this combined file. Add the exact prebuilt
# AndroidX component attribution so recipients can associate the bundled
# Apache-2.0 terms with the native library in their APK.
python3 - "$notices/THIRD-PARTY-NOTICES.txt" "$notices/manifest.json" <<'PY'
import hashlib
import json
import os
import pathlib
import stat
import sys

combined_path = pathlib.Path(sys.argv[1])
manifest_path = pathlib.Path(sys.argv[2])
for path in (combined_path, manifest_path):
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise SystemExit("generated Android notice output is unsafe")
attribution = (
    b"\n===== Android Graphics Path runtime dependency =====\n"
    b"androidx.graphics:graphics-path:1.0.1\n"
    b"Publisher: The Android Open Source Project\n"
    b"License: Apache-2.0 (full terms included in this combined notice)\n"
    b"License metadata: https://dl.google.com/dl/android/maven2/androidx/graphics/graphics-path/1.0.1/graphics-path-1.0.1.pom\n"
    b"AAR SHA-256: 8ca4032b6d79b351f0b59ad4b580eddbb9423e1652f7c958830687f1eee2ec03\n"
)
combined = combined_path.read_bytes()
manifest = json.loads(manifest_path.read_text())
descriptor = manifest.get("combinedNotice", {})
if descriptor != {
    "bundlePath": "THIRD-PARTY-NOTICES.txt",
    "bytes": len(combined),
    "sha256": hashlib.sha256(combined).hexdigest(),
}:
    raise SystemExit("generated Android notice descriptor differs before attribution")
if attribution in combined:
    raise SystemExit("generated Android notice already contains the runtime attribution")
combined += attribution
if len(combined) > 8 * 1024 * 1024:
    raise SystemExit("attributed Android notice exceeds the viewer bound")
manifest["recipientAttributions"] = [{
    "artifactSha256": "8ca4032b6d79b351f0b59ad4b580eddbb9423e1652f7c958830687f1eee2ec03",
    "coordinate": "androidx.graphics:graphics-path:1.0.1",
    "license": "Apache-2.0",
    "licenseEvidence": "https://dl.google.com/dl/android/maven2/androidx/graphics/graphics-path/1.0.1/graphics-path-1.0.1.pom",
    "name": "Android Graphics Path",
    "publisher": "The Android Open Source Project",
}]
manifest["combinedNotice"] = {
    "bundlePath": "THIRD-PARTY-NOTICES.txt",
    "bytes": len(combined),
    "sha256": hashlib.sha256(combined).hexdigest(),
}
bounds = manifest.get("bounds")
if not isinstance(bounds, dict) or not isinstance(bounds.get("bytes"), int):
    raise SystemExit("generated Android notice bounds are malformed")
bounds["bytes"] += len(attribution)
if bounds["bytes"] > 64 * 1024 * 1024:
    raise SystemExit("attributed Android notice bundle exceeds its bound")
manifest_bytes = (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()
if len(manifest_bytes) > 4 * 1024 * 1024:
    raise SystemExit("attributed Android notice manifest exceeds the viewer bound")

def replace_read_only(path, data):
    os.chmod(path, 0o644)
    try:
        path.write_bytes(data)
    finally:
        os.chmod(path, 0o444)

replace_read_only(combined_path, combined)
replace_read_only(manifest_path, manifest_bytes)
PY

python3 - "$reports_root" "$notices/manifest.json" "$hash_file" "$guardian_hash_file" <<'PY'
import hashlib, json, pathlib, sys
reports = pathlib.Path(sys.argv[1])
notice_manifest = json.loads(pathlib.Path(sys.argv[2]).read_text())
binary_manifests = {}
for component, manifest_path in (("rclone", pathlib.Path(sys.argv[3])), ("guardian", pathlib.Path(sys.argv[4]))):
    parsed = {}
    for line in manifest_path.read_text().splitlines():
        abi, digest, size_text = line.split(" ")
        parsed[abi] = (int(size_text), digest)
    if set(parsed) != {"arm64-v8a", "x86_64"}:
        raise SystemExit("native binary hash manifest is incomplete")
    binary_manifests[component] = parsed
expected_notices = {row["sourceName"]: (row["bytes"], row["sha256"]) for row in notice_manifest.get("toolchainNotices", [])}
if set(expected_notices) != {"NOTICE", "NOTICE.toolchain"}:
    raise SystemExit("Android notice manifest lacks the exact NDK notice pair")
rows = []
for component in ("guardian", "rclone"):
    for abi in ("arm64-v8a", "x86_64"):
        path = reports / f"{component}-link-provenance-{abi}.json"
        raw = path.read_bytes()
        value = json.loads(raw)
        observed = {name: (record["bytes"], record["sha256"]) for name, record in value["ndk"]["files"].items() if name in {"NOTICE", "NOTICE.toolchain"}}
        if observed != expected_notices:
            raise SystemExit("NDK link and notice evidence disagree")
        binary = value["evidence"]["binary"]
        if (binary["bytes"], binary["sha256"]) != binary_manifests[component][abi]:
            raise SystemExit("native binary and link provenance evidence disagree")
        rows.append({"abi": abi, "component": component, "manifestBytes": len(raw), "manifestSha256": hashlib.sha256(raw).hexdigest()})
(reports / "android-sync-engine-link-provenance-index.json").write_text(json.dumps({"schemaVersion": 1, "status": "link-inputs-classified-review-required", "records": rows}, indent=2, sort_keys=True) + "\n")
PY

combined="$notices/THIRD-PARTY-NOTICES.txt"
notice_manifest="$notices/manifest.json"
combined_size=$(size_of "$combined")
manifest_size=$(size_of "$notice_manifest")
test "$combined_size" -gt 0 && test "$combined_size" -le "$maximum_notice_bytes" || fail "Combined Android notices exceed the viewer bound"
test "$manifest_size" -gt 0 && test "$manifest_size" -le "$maximum_notice_manifest_bytes" || fail "Android notice manifest exceeds the viewer bound"
combined_sha=$(sha256 "$combined")
manifest_sha=$(sha256 "$notice_manifest")
cat > "$assets_root/sync-engine-notices-index.txt" <<EOF
1
combined $combined_sha $combined_size sync-engine-notices/THIRD-PARTY-NOTICES.txt
manifest $manifest_sha $manifest_size sync-engine-notices/manifest.json
EOF
cat > "$reports_root/source-build.json" <<EOF
{"schema":1,"engine":{"name":"rclone","version":"$engine_version","upstreamCommit":"$engine_commit","upstreamSourceState":"unmodified","wrapperSourceTree":"$source_tree"},"target":{"goos":"android","cgoEnabled":true,"api":26,"buildMode":"pie","maximumPageSize":16384,"buildTags":[]},"toolchain":{"goVersion":"go1.26.7","ndkVersion":"$expected_ndk"}}
EOF
test "$(sha256 "$build_source/go.mod")" = "$expected_gomod_sha" && test "$(sha256 "$build_source/go.sum")" = "$expected_gosum_sha" || fail "rclone module files changed during Android build"
test "$(git -C "$repo_root" rev-parse HEAD)" = "$source_commit" || fail "repository commit changed during Android build"
echo "  Android target notices: $combined_size bytes, sha256=$combined_sha"
