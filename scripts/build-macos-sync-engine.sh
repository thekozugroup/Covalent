#!/bin/sh
# Build the pinned macOS arm64 folder worker and exact target notice bundle.
set -eu

test "$#" -eq 2 || {
  echo "usage: $0 /exact/syncthing/checkout /new/output" >&2
  exit 64
}

source_dir=$1
output_root=$2
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
guardian_source="$repo_root/packaging/sync-engine/engine-guardian.c"
inventory_tool="$repo_root/scripts/collect-go-target-license-inventory.py"
notice_tool="$repo_root/scripts/collect-sync-engine-notices.py"
project_license="$repo_root/LICENSE"
ofl_license="$repo_root/docs/licenses/sync-engine/OFL-1.1.txt"

engine_version=v2.1.3
engine_commit=946e2b83a1f6c6ae119427c09e0a5802940b82ff
upstream_archive_sha=dbcc9498602286a843f29a7104833bd1422082999aa51ff92eef493172d47959
source_export_sha=eb60efd57d1662af75ffb2f7b89abab7200362c654838486138a34bee00fed29
go_mod_sha=a129d6ae9cf20593fab4b1fb04ac09b176c4942d3a4bec9394f9c888fe2d1bd1
go_sum_sha=7e9606117eca33e9263181a3d0141e403c940c55022a061d8ed9e22d4bda2acd
guardian_sha=c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579
source_date_epoch=1785792965
maximum_graph_bytes=$((16 * 1024 * 1024))
maximum_inventory_bytes=$((16 * 1024 * 1024))
maximum_notice_bytes=$((8 * 1024 * 1024))
maximum_notice_manifest_bytes=$((4 * 1024 * 1024))

sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
size_of() { wc -c < "$1" | tr -d '[:space:]'; }
fail() { echo "$1" >&2; exit 1; }

test "$(uname -s)" = Darwin && test "$(uname -m)" = arm64 ||
  fail "macOS sync engine must be built on Apple Silicon macOS"
case "$source_dir:$output_root" in /*:/*) ;; *) fail "source and output paths must be absolute" ;; esac
test -x "$inventory_tool" && test -x "$notice_tool" || fail "notice collectors are unavailable"
test "$(sha256 "$guardian_source")" = "$guardian_sha" || fail "guardian source digest differs"
git -C "$source_dir" rev-parse --is-inside-work-tree >/dev/null 2>&1 ||
  fail "Syncthing source must be a git checkout"
test "$(git -C "$source_dir" rev-parse HEAD)" = "$engine_commit" ||
  fail "Syncthing source commit differs"
caller_state_before=$(git -C "$source_dir" status --porcelain=v1 --untracked-files=all)
test "$(sha256 "$source_dir/go.mod")" = "$go_mod_sha" &&
  test "$(sha256 "$source_dir/go.sum")" = "$go_sum_sha" ||
  fail "Syncthing module graph differs"

test ! -e "$output_root" && test ! -L "$output_root" || fail "output must be a new path"
test -d "$(dirname -- "$output_root")" || fail "output parent is unavailable"
mkdir "$output_root"
output_root=$(CDPATH='' cd -- "$output_root" && pwd -P)
private="$output_root/.private-build"
mkdir "$private"

go_bin=${COVALENT_GO_BIN:-}
cleanup() {
  if test -d "$private/gomodcache" && test -n "$go_bin" && test -x "$go_bin"; then
    GOMODCACHE="$private/gomodcache" GOPATH="$private/gopath" GOTOOLCHAIN=local \
      "$go_bin" clean -modcache >/dev/null 2>&1 || true
  fi
  rm -rf "$private"
}
# Install cleanup before archive validation, extraction, or toolchain probing.
trap cleanup EXIT HUP INT TERM
if test -n "${COVALENT_GO_ARCHIVE:-}"; then
  go_archive=$COVALENT_GO_ARCHIVE
  test -f "$go_archive" && test ! -L "$go_archive" &&
    test "$(size_of "$go_archive")" -eq 64772572 &&
    test "$(sha256 "$go_archive")" = 020a1e8224811be75163e920bc77e0926a1390a6aeea19bdcf23f74b9d749f6d ||
    fail "Go 1.26.7 archive digest or size differs"
  if tar -tzf "$go_archive" | grep -Eq '(^/|(^|/)\.\.(/|$))'; then
    fail "Go archive contains an unsafe member"
  fi
  mkdir "$private/toolchain"
  tar -xzf "$go_archive" -C "$private/toolchain"
  go_bin="$private/toolchain/go/bin/go"
fi
if test -z "$go_bin"; then go_bin=$(command -v go || true); fi
test -n "$go_bin" && test -x "$go_bin" || fail "set COVALENT_GO_BIN to exact Go 1.26.7"
go_bin=$(CDPATH='' cd -- "$(dirname -- "$go_bin")" && pwd -P)/$(basename -- "$go_bin")
test "$($go_bin version)" = "go version go1.26.7 darwin/arm64" ||
  fail "macOS sync engine requires exact Go 1.26.7 darwin/arm64"
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
git -C "$source_dir" archive --format=tar "$engine_commit" > "$source_export"
test "$(sha256 "$source_export")" = "$source_export_sha" ||
  fail "Syncthing source export digest differs"
tar -xf "$source_export" -C "$build_source"
test "$(sha256 "$build_source/go.mod")" = "$go_mod_sha" &&
  test "$(sha256 "$build_source/go.sum")" = "$go_sum_sha" ||
  fail "exported Syncthing module graph differs"

worker="$output_root/covalent-syncthing"
graph="$output_root/go-target-deps.ndjson"
inventory="$output_root/target-license-inventory.json"
notices="$output_root/notices"
macos_sdk=$(xcrun --sdk macosx --show-sdk-path)
test -d "$macos_sdk" || fail "macOS SDK is unavailable"
macos_sdk=$(CDPATH='' cd -- "$macos_sdk" && pwd -P)
cgo_cflags="-isysroot $macos_sdk -mmacosx-version-min=15.0 -fdebug-prefix-map=$private=/covalent-build -ffile-prefix-map=$private=/covalent-build -fmacro-prefix-map=$private=/covalent-build"
cgo_ldflags="-isysroot $macos_sdk -mmacosx-version-min=15.0"
(
  cd "$build_source"
  env BUILD_HOST=covalent-macos-release BUILD_USER=covalent-macos-release \
    CGO_ENABLED=1 GOOS=darwin GOARCH=arm64 \
    CC="$(xcrun --find clang)" \
    SDKROOT="$macos_sdk" CGO_CFLAGS="$cgo_cflags" CGO_LDFLAGS="$cgo_ldflags" \
    SOURCE_DATE_EPOCH="$source_date_epoch" \
    "$go_bin" run -mod=readonly build.go \
      -goos darwin -goarch arm64 -gocmd "$go_bin" -no-upgrade \
      -version "$engine_version" -build-out "$worker" build syncthing
  env CGO_ENABLED=1 GOOS=darwin GOARCH=arm64 \
    CC="$(xcrun --find clang)" \
    SDKROOT="$macos_sdk" CGO_CFLAGS="$cgo_cflags" CGO_LDFLAGS="$cgo_ldflags" \
    "$go_bin" list -tags noupgrade -deps -json ./cmd/syncthing > "$graph"
  "$go_bin" mod verify
)

test -f "$worker" && test ! -L "$worker" && test -x "$worker" || fail "worker build is incomplete"
test "$(xcrun lipo -archs "$worker")" = arm64 || fail "worker is not arm64-only"
graph_bytes=$(size_of "$graph")
test "$graph_bytes" -gt 0 && test "$graph_bytes" -le "$maximum_graph_bytes" ||
  fail "target dependency graph is outside its byte bound"
"$go_bin" version -m "$worker" > "$output_root/go-version.txt"
grep -Fq 'go1.26.7' "$output_root/go-version.txt" || fail "worker Go version differs"
grep -Eq '^[[:space:]]*path[[:space:]]+github\.com/syncthing/syncthing/cmd/syncthing$' \
  "$output_root/go-version.txt" || fail "worker module path differs"
worker_version=$("$worker" version)
case "$worker_version" in
  "syncthing v2.1.3 "*) ;;
  *) fail "built worker cannot execute or reports a different version" ;;
esac

python3 "$inventory_tool" \
  --source-root "$build_source" --module-cache "$GOMODCACHE" \
  --target macos-arm64 darwin arm64 1 noupgrade "$graph" \
  --output "$inventory"
inventory_bytes=$(size_of "$inventory")
test "$inventory_bytes" -gt 0 && test "$inventory_bytes" -le "$maximum_inventory_bytes" ||
  fail "target inventory is outside its byte bound"
python3 "$notice_tool" \
  --inventory "$inventory" --source-root "$build_source" \
  --module-cache "$GOMODCACHE" --go-root "$go_root" \
  --guardian-source "$guardian_source" --project-license "$project_license" \
  --ofl-license "$ofl_license" --output "$notices"
cp "$build_source/LICENSE" "$output_root/Syncthing-LICENSE.txt"
cp "$build_source/AUTHORS" "$output_root/Syncthing-AUTHORS.txt"

combined="$notices/THIRD-PARTY-NOTICES.txt"
notice_manifest="$notices/manifest.json"
combined_bytes=$(size_of "$combined")
manifest_bytes=$(size_of "$notice_manifest")
test "$combined_bytes" -gt 0 && test "$combined_bytes" -le "$maximum_notice_bytes" ||
  fail "combined notices are outside the viewer bound"
test "$manifest_bytes" -gt 0 && test "$manifest_bytes" -le "$maximum_notice_manifest_bytes" ||
  fail "notice manifest is outside its viewer bound"

worker_sha=$(sha256 "$worker")
inventory_sha=$(sha256 "$inventory")
notice_sha=$(sha256 "$combined")
notice_manifest_sha=$(sha256 "$notice_manifest")
cat > "$output_root/source-build.json" <<EOF
{"schema":1,"engine":{"version":"$engine_version","commit":"$engine_commit","upstreamArchiveSha256":"$upstream_archive_sha","sourceExportSha256":"$source_export_sha"},"target":{"goos":"darwin","goarch":"arm64","cgoEnabled":true,"minimumMacOS":"15.0","buildTags":["noupgrade"]},"toolchain":{"goVersion":"go1.26.7"},"evidence":{"licenseInventorySha256":"$inventory_sha","licenseInventoryBytes":$inventory_bytes,"noticeManifestSha256":"$notice_manifest_sha","combinedNoticeSha256":"$notice_sha","combinedNoticeBytes":$combined_bytes},"unsignedExecutable":{"sha256":"$worker_sha","bytes":$(size_of "$worker")}}
EOF
chmod 0555 "$worker"
find "$notices" -type d -exec chmod 0555 {} +
find "$notices" -type f -exec chmod 0444 {} +
chmod 0444 "$graph" "$inventory" "$output_root/go-version.txt" \
  "$output_root/source-build.json" "$output_root/Syncthing-LICENSE.txt" \
  "$output_root/Syncthing-AUTHORS.txt"

test "$(sha256 "$build_source/go.mod")" = "$go_mod_sha" &&
  test "$(sha256 "$build_source/go.sum")" = "$go_sum_sha" ||
  fail "Syncthing module files changed during build"
test "$(git -C "$source_dir" rev-parse HEAD)" = "$engine_commit" &&
  test "$(git -C "$source_dir" status --porcelain=v1 --untracked-files=all)" = "$caller_state_before" ||
  fail "caller Syncthing checkout changed during build"
echo "macOS maintained engine: $worker_sha ($combined_bytes notice bytes)"
