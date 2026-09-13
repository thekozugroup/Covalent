#!/bin/sh
# Build the restricted macOS arm64 rclone worker and exact target notice bundle.
set -eu

test "$#" -eq 2 || { echo "usage: $0 /repo/packaging/rclone /new/output" >&2; exit 64; }
source_dir=$1
output_root=$2
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
expected_source="$repo_root/packaging/rclone"
guardian_source="$repo_root/packaging/sync-engine/engine-guardian.c"
inventory_tool="$repo_root/scripts/collect-go-target-license-inventory.py"
notice_tool="$repo_root/scripts/collect-sync-engine-notices.py"
project_license="$repo_root/LICENSE"

engine_version=v1.75.1
engine_commit=687d264b689b8c49a67e2e52a8a5e0caa01c04ce
go_mod_sha=1708132fb012d15c89e7863a79abea50c66895e78d8b85037275db213c1d103f
go_sum_sha=801fcfc81dd2f84417d5410c04aa4fd5936387246372eaf8920c2b0492ffa89b
rclone_license_sha=9266eae9c6a441de0f6847f19ac8f09b280d55612b079eca03b3d49b822c1a71
guardian_sha=c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579
source_date_epoch=1785792965
minimum_worker_bytes=$((8 * 1024 * 1024))
maximum_worker_bytes=$((48 * 1024 * 1024))
maximum_graph_bytes=$((16 * 1024 * 1024))
maximum_inventory_bytes=$((16 * 1024 * 1024))
maximum_notice_bytes=$((8 * 1024 * 1024))
maximum_notice_manifest_bytes=$((4 * 1024 * 1024))

sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
size_of() { wc -c < "$1" | tr -d '[:space:]'; }
fail() { echo "$1" >&2; exit 1; }

test "$(uname -s)" = Darwin && test "$(uname -m)" = arm64 || fail "macOS sync engine must be built on Apple Silicon macOS"
case "$source_dir:$output_root" in /*:/*) ;; *) fail "source and output paths must be absolute" ;; esac
source_dir=$(CDPATH='' cd -- "$source_dir" && pwd -P)
test "$source_dir" = "$expected_source" || fail "worker source must be the repository packaging/rclone module"
test -x "$inventory_tool" && test -x "$notice_tool" || fail "notice collectors are unavailable"
test "$(sha256 "$guardian_source")" = "$guardian_sha" || fail "guardian source digest differs"
test "$(sha256 "$source_dir/go.mod")" = "$go_mod_sha" && \
  test "$(sha256 "$source_dir/go.sum")" = "$go_sum_sha" && \
  test "$(sha256 "$source_dir/LICENSE.rclone")" = "$rclone_license_sha" ||
  fail "restricted rclone module differs"
source_commit=$(git -C "$repo_root" rev-parse HEAD)
source_tree=$(git -C "$repo_root" rev-parse HEAD:packaging/rclone)

test ! -e "$output_root" && test ! -L "$output_root" || fail "output must be a new path"
test -d "$(dirname -- "$output_root")" || fail "output parent is unavailable"
mkdir "$output_root"
output_root=$(CDPATH='' cd -- "$output_root" && pwd -P)
private="$output_root/.private-build"
mkdir "$private"

go_bin=${COVALENT_GO_BIN:-}
cleanup() {
  if test -d "$private/gomodcache" && test -n "$go_bin" && test -x "$go_bin"; then
    GOMODCACHE="$private/gomodcache" GOPATH="$private/gopath" GOTOOLCHAIN=local "$go_bin" clean -modcache >/dev/null 2>&1 || true
  fi
  rm -rf "$private"
}
trap cleanup EXIT HUP INT TERM
if test -n "${COVALENT_GO_ARCHIVE:-}"; then
  go_archive=$COVALENT_GO_ARCHIVE
  test -f "$go_archive" && test ! -L "$go_archive" && test "$(size_of "$go_archive")" -eq 64772572 && \
    test "$(sha256 "$go_archive")" = 020a1e8224811be75163e920bc77e0926a1390a6aeea19bdcf23f74b9d749f6d ||
    fail "Go 1.26.7 archive digest or size differs"
  if tar -tzf "$go_archive" | grep -Eq '(^/|(^|/)\.\.(/|$))'; then fail "Go archive contains an unsafe member"; fi
  mkdir "$private/toolchain"
  tar -xzf "$go_archive" -C "$private/toolchain"
  go_bin="$private/toolchain/go/bin/go"
fi
if test -z "$go_bin"; then go_bin=$(command -v go || true); fi
test -n "$go_bin" && test -x "$go_bin" || fail "set COVALENT_GO_BIN to exact Go 1.26.7"
go_bin=$(CDPATH='' cd -- "$(dirname -- "$go_bin")" && pwd -P)/$(basename -- "$go_bin")
test "$($go_bin version)" = "go version go1.26.7 darwin/arm64" || fail "macOS sync engine requires exact Go 1.26.7 darwin/arm64"
go_root=$($go_bin env GOROOT)
test -d "$go_root" && test ! -L "$go_root" || fail "Go root is unsafe"

export GOPATH="$private/gopath"
export GOMODCACHE="$private/gomodcache"
export GOCACHE="$private/gocache"
export GOFLAGS='-buildvcs=false -mod=readonly -trimpath'
export GOTOOLCHAIN=local
mkdir "$GOPATH" "$GOMODCACHE" "$GOCACHE"
build_source="$private/source-export"
mkdir "$build_source"
source_export="$private/source-export.tar"
git -C "$repo_root" archive --format=tar HEAD:packaging/rclone > "$source_export"
tar -xf "$source_export" -C "$build_source"
test "$(sha256 "$build_source/go.mod")" = "$go_mod_sha" && \
  test "$(sha256 "$build_source/go.sum")" = "$go_sum_sha" && \
  test "$(sha256 "$build_source/LICENSE.rclone")" = "$rclone_license_sha" ||
  fail "exported restricted rclone module differs"

worker="$output_root/covalent-rclone"
graph="$output_root/go-target-deps.ndjson"
inventory="$output_root/target-license-inventory.json"
notices="$output_root/notices"
(
  cd "$build_source"
  env CGO_ENABLED=0 GOOS=darwin GOARCH=arm64 SOURCE_DATE_EPOCH="$source_date_epoch" \
    "$go_bin" build -ldflags "-s -w -X github.com/rclone/rclone/fs.Version=$engine_version" -o "$worker" .
  env CGO_ENABLED=0 GOOS=darwin GOARCH=arm64 "$go_bin" list -deps -json . > "$graph"
  "$go_bin" mod verify
)
test -f "$worker" && test ! -L "$worker" && test -x "$worker" || fail "worker build is incomplete"
test "$(xcrun lipo -archs "$worker")" = arm64 || fail "worker is not arm64-only"
worker_bytes=$(size_of "$worker")
test "$worker_bytes" -ge "$minimum_worker_bytes" && test "$worker_bytes" -le "$maximum_worker_bytes" || fail "worker size is outside its reviewed bound"
graph_bytes=$(size_of "$graph")
test "$graph_bytes" -gt 0 && test "$graph_bytes" -le "$maximum_graph_bytes" || fail "target dependency graph is outside its byte bound"
"$go_bin" version -m "$worker" > "$output_root/go-version.txt"
grep -Fq 'go1.26.7' "$output_root/go-version.txt" || fail "worker Go version differs"
grep -Eq '^[[:space:]]*path[[:space:]]+github\.com/thekozugroup/Covalent/packaging/rclone$' "$output_root/go-version.txt" || fail "worker module path differs"
"$worker" version | grep -Fq "rclone $engine_version" || fail "built worker reports a different version"

python3 "$inventory_tool" --source-root "$build_source" --module-cache "$GOMODCACHE" \
  --target macos-arm64 darwin arm64 0 - "$graph" --output "$inventory"
inventory_bytes=$(size_of "$inventory")
test "$inventory_bytes" -gt 0 && test "$inventory_bytes" -le "$maximum_inventory_bytes" || fail "target inventory is outside its byte bound"
python3 "$notice_tool" --inventory "$inventory" --source-root "$build_source" \
  --module-cache "$GOMODCACHE" --go-root "$go_root" --guardian-source "$guardian_source" \
  --project-license "$project_license" --output "$notices"
cp "$build_source/LICENSE.rclone" "$output_root/rclone-LICENSE.txt"

combined="$notices/THIRD-PARTY-NOTICES.txt"
notice_manifest="$notices/manifest.json"
combined_bytes=$(size_of "$combined")
manifest_bytes=$(size_of "$notice_manifest")
test "$combined_bytes" -gt 0 && test "$combined_bytes" -le "$maximum_notice_bytes" || fail "combined notices are outside the viewer bound"
test "$manifest_bytes" -gt 0 && test "$manifest_bytes" -le "$maximum_notice_manifest_bytes" || fail "notice manifest is outside the viewer bound"
worker_sha=$(sha256 "$worker")
inventory_sha=$(sha256 "$inventory")
notice_sha=$(sha256 "$combined")
notice_manifest_sha=$(sha256 "$notice_manifest")
cat > "$output_root/source-build.json" <<EOF
{"schema":1,"engine":{"name":"rclone","version":"$engine_version","upstreamCommit":"$engine_commit","upstreamSourceState":"unmodified","wrapperSourceTree":"$source_tree"},"target":{"goos":"darwin","goarch":"arm64","cgoEnabled":false,"buildTags":[]},"toolchain":{"goVersion":"go1.26.7"},"evidence":{"licenseInventorySha256":"$inventory_sha","licenseInventoryBytes":$inventory_bytes,"noticeManifestSha256":"$notice_manifest_sha","combinedNoticeSha256":"$notice_sha","combinedNoticeBytes":$combined_bytes},"unsignedExecutable":{"sha256":"$worker_sha","bytes":$worker_bytes}}
EOF
chmod 0555 "$worker"
find "$notices" -type d -exec chmod 0555 {} +
find "$notices" -type f -exec chmod 0444 {} +
chmod 0444 "$graph" "$inventory" "$output_root/go-version.txt" "$output_root/source-build.json" "$output_root/rclone-LICENSE.txt"
test "$(sha256 "$build_source/go.mod")" = "$go_mod_sha" && test "$(sha256 "$build_source/go.sum")" = "$go_sum_sha" || fail "rclone module files changed during build"
test "$(git -C "$repo_root" rev-parse HEAD)" = "$source_commit" || fail "repository commit changed during build"
echo "macOS maintained engine: $worker_sha ($combined_bytes notice bytes)"
