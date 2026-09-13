#!/bin/sh
# Build the restricted rclone worker and package it with the reviewed guardian.
set -eu

ENGINE_VERSION=v1.75.1
ENGINE_COMMIT=687d264b689b8c49a67e2e52a8a5e0caa01c04ce
GO_MOD_SHA256=1708132fb012d15c89e7863a79abea50c66895e78d8b85037275db213c1d103f
GO_SUM_SHA256=801fcfc81dd2f84417d5410c04aa4fd5936387246372eaf8920c2b0492ffa89b
RCLONE_LICENSE_SHA256=9266eae9c6a441de0f6847f19ac8f09b280d55612b079eca03b3d49b822c1a71
GUARDIAN_SOURCE_SHA256=c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579
MIN_WORKER_BYTES=$((8 * 1024 * 1024))
MAX_WORKER_BYTES=$((48 * 1024 * 1024))
MIN_GUARDIAN_BYTES=$((8 * 1024))
MAX_GUARDIAN_BYTES=$((128 * 1024))
MAX_TARGET_GRAPH_BYTES=$((16 * 1024 * 1024))
MAX_TARGET_INVENTORY_BYTES=$((16 * 1024 * 1024))
MAX_GO_VERSION_BYTES=$((1024 * 1024))
MAX_NOTICE_BUNDLE_BYTES=$((80 * 1024 * 1024))

sha256() { sha256sum "$1" | awk '{print $1}'; }
size_of() { wc -c < "$1" | tr -d '[:space:]'; }
fail() { echo "$1" >&2; exit 1; }

require_hash() {
  test "$(sha256 "$1")" = "$2" || fail "Maintained engine source evidence failed its pinned digest"
}

architecture() {
  case "$1" in
    arm64) printf '%s\n' aarch64 ;;
    amd64) printf '%s\n' x86_64 ;;
    *) echo "Unsupported Linux engine architecture: $1" >&2; exit 64 ;;
  esac
}

build_worker() {
  test "$#" -eq 3 || { echo "usage: $0 worker MODULE-SOURCE OUTPUT TARGETARCH" >&2; exit 64; }
  source_dir=$1
  output_root=$2
  target_arch=$3
  case "$source_dir:$output_root" in /*:/*) ;; *) fail "Worker source and output paths must be absolute" ;; esac
  runtime_arch=$(architecture "$target_arch")
  test "$(uname -s)" = Linux && test "$(uname -m)" = "$runtime_arch" || {
    echo "Worker must be built in its exact target-platform container" >&2
    exit 69
  }
  test "$(go version)" = "go version go1.26.7 linux/$target_arch" || {
    echo "Linux engine build requires exact Go 1.26.7 for $target_arch" >&2
    exit 69
  }
  test -f "$source_dir/main.go" && test -f "$source_dir/go.mod" && \
    test -f "$source_dir/go.sum" && test -f "$source_dir/LICENSE.rclone" ||
    fail "Restricted rclone module source is incomplete"
  require_hash "$source_dir/go.mod" "$GO_MOD_SHA256"
  require_hash "$source_dir/go.sum" "$GO_SUM_SHA256"
  require_hash "$source_dir/LICENSE.rclone" "$RCLONE_LICENSE_SHA256"
  go_mod_before=$(sha256 "$source_dir/go.mod")
  go_sum_before=$(sha256 "$source_dir/go.sum")
  mkdir -p "$output_root"
  output="$output_root/covalent-rclone"
  (
    cd "$source_dir"
    env CGO_ENABLED=0 GOFLAGS='-buildvcs=false -mod=readonly -trimpath' \
      GOOS=linux GOARCH="$target_arch" GOTOOLCHAIN=local SOURCE_DATE_EPOCH=1785792965 \
      go build -ldflags "-s -w -X github.com/rclone/rclone/fs.Version=$ENGINE_VERSION" \
        -o "$output" .
    env CGO_ENABLED=0 GOFLAGS='-buildvcs=false -mod=readonly -trimpath' \
      GOOS=linux GOARCH="$target_arch" GOTOOLCHAIN=local \
      go list -deps -json . > "$output_root/go-target-deps.ndjson"
    env GOFLAGS=-mod=readonly GOTOOLCHAIN=local go mod verify
  )
  test "$go_mod_before" = "$(sha256 "$source_dir/go.mod")" && \
    test "$go_sum_before" = "$(sha256 "$source_dir/go.sum")" ||
    fail "Restricted rclone build changed its pinned module graph"
  worker_size=$(size_of "$output")
  test "$worker_size" -ge "$MIN_WORKER_BYTES" && test "$worker_size" -le "$MAX_WORKER_BYTES" ||
    fail "Linux engine worker size is outside its reviewed bound"
  go version -m "$output" > "$output_root/go-version.txt"
  grep -Fq 'go1.26.7' "$output_root/go-version.txt"
  grep -Eq '^[[:space:]]*path[[:space:]]+github\.com/thekozugroup/Covalent/packaging/rclone$' \
    "$output_root/go-version.txt"
  graph_size=$(size_of "$output_root/go-target-deps.ndjson")
  test "$graph_size" -gt 0 && test "$graph_size" -le "$MAX_TARGET_GRAPH_BYTES" ||
    fail "Linux target dependency graph is outside its retained byte bound"
  test "$(size_of "$output_root/go-version.txt")" -gt 0 && \
    test "$(size_of "$output_root/go-version.txt")" -le "$MAX_GO_VERSION_BYTES" ||
    fail "Linux worker build metadata is outside its retained byte bound"
  "$output" version | grep -Fq "rclone $ENGINE_VERSION"
  install -m 0444 "$source_dir/LICENSE.rclone" "$output_root/rclone-LICENSE.txt"
  chmod 0555 "$output"
}

package_engine() {
  test "$#" -eq 5 || { echo "usage: $0 package WORKER-OUTPUT GUARDIAN-SOURCE NOTICES OUTPUT TARGETARCH" >&2; exit 64; }
  worker_root=$1
  guardian_source=$2
  notices=$3
  output_root=$4
  target_arch=$5
  runtime_arch=$(architecture "$target_arch")
  case "$target_arch" in arm64) expected_machine=AArch64 ;; amd64) expected_machine='Advanced Micro Devices X86-64' ;; esac
  test "$(uname -s)" = Linux && test "$(uname -m)" = "$runtime_arch" || {
    echo "Engine package must be assembled in its exact target-platform container" >&2
    exit 69
  }
  require_hash "$guardian_source" "$GUARDIAN_SOURCE_SHA256"
  require_hash "$worker_root/rclone-LICENSE.txt" "$RCLONE_LICENSE_SHA256"
  for evidence in go-target-deps.ndjson go-version.txt target-license-inventory.json; do
    test -f "$worker_root/$evidence" && test ! -L "$worker_root/$evidence" || fail "Target dependency evidence is incomplete"
  done
  graph_size=$(size_of "$worker_root/go-target-deps.ndjson")
  go_version_size=$(size_of "$worker_root/go-version.txt")
  inventory_size=$(size_of "$worker_root/target-license-inventory.json")
  test "$graph_size" -gt 0 && test "$graph_size" -le "$MAX_TARGET_GRAPH_BYTES" && \
    test "$go_version_size" -gt 0 && test "$go_version_size" -le "$MAX_GO_VERSION_BYTES" && \
    test "$inventory_size" -gt 0 && test "$inventory_size" -le "$MAX_TARGET_INVENTORY_BYTES" ||
    fail "Target dependency evidence is outside its retained byte bound"
  graph_sha=$(sha256 "$worker_root/go-target-deps.ndjson")
  go_version_sha=$(sha256 "$worker_root/go-version.txt")
  inventory_sha=$(sha256 "$worker_root/target-license-inventory.json")
  test "$(grep -Fc '"status": "evidence-collected-review-required"' "$worker_root/target-license-inventory.json")" -eq 1 && \
    test "$(grep -Fc '"goos": "linux"' "$worker_root/target-license-inventory.json")" -eq 1 && \
    test "$(grep -Fc "\"goarch\": \"$target_arch\"" "$worker_root/target-license-inventory.json")" -eq 1 && \
    test "$(grep -Fc '"cgoEnabled": false' "$worker_root/target-license-inventory.json")" -eq 1 && \
    test "$(grep -Fc '"buildTags": []' "$worker_root/target-license-inventory.json")" -eq 1 ||
    fail "Target dependency inventory does not bind the exact Linux package"
  test -d "$notices" && test ! -L "$notices" && \
    test -f "$notices/manifest.json" && test ! -L "$notices/manifest.json" && \
    test -f "$notices/THIRD-PARTY-NOTICES.txt" && test ! -L "$notices/THIRD-PARTY-NOTICES.txt" ||
    fail "Target notice bundle is incomplete"
  notice_files=$(find "$notices" -type f | wc -l | tr -d '[:space:]')
  notice_links=$(find "$notices" -type l | wc -l | tr -d '[:space:]')
  notice_other=$(find "$notices" ! -type d ! -type f | wc -l | tr -d '[:space:]')
  notice_bytes=$(find "$notices" -type f -exec sh -c 'for file do wc -c < "$file"; done' sh {} + | awk '{ total += $1 } END { print total + 0 }')
  test "$notice_files" -ge 1 && test "$notice_files" -le 8192 && test "$notice_links" -eq 0 && \
    test "$notice_other" -eq 0 && test "$notice_bytes" -le "$MAX_NOTICE_BUNDLE_BYTES" ||
    fail "Target notice bundle entries are outside their reviewed bound"
  install -d -m 0755 "$output_root/bin" "$output_root/share"
  worker="$output_root/bin/covalent-rclone"
  guardian="$output_root/bin/covalent-engine-guardian"
  install -m 0555 "$worker_root/covalent-rclone" "$worker"
  cc -std=c11 -Wall -Wextra -Werror -Os -D_FORTIFY_SOURCE=2 -fstack-protector-strong \
    -fPIE -static-pie -Wl,-s,-z,relro,-z,now,-z,noexecstack "$guardian_source" -o "$guardian"
  chmod 0555 "$guardian"
  worker_size=$(size_of "$worker")
  guardian_size=$(size_of "$guardian")
  printf 'Linux engine package bytes: worker=%s guardian=%s\n' "$worker_size" "$guardian_size"
  test "$worker_size" -ge "$MIN_WORKER_BYTES" && test "$worker_size" -le "$MAX_WORKER_BYTES" || fail "Linux worker exceeds its reviewed package size range"
  test "$guardian_size" -ge "$MIN_GUARDIAN_BYTES" && test "$guardian_size" -le "$MAX_GUARDIAN_BYTES" || fail "Linux guardian exceeds its reviewed package size range"
  readelf=$(cc -print-prog-name=readelf)
  command -v "$readelf" >/dev/null 2>&1 || { echo "The pinned Linux builder does not provide readelf" >&2; exit 69; }
  for executable in "$worker" "$guardian"; do
    evidence_name=$(basename "$executable")
    "$readelf" -hW "$executable" > "$output_root/share/$evidence_name.elf-header"
    "$readelf" -dW "$executable" > "$output_root/share/$evidence_name.elf-dynamic"
    "$readelf" -lW "$executable" > "$output_root/share/$evidence_name.elf-program"
    if grep -Eq 'NEEDED|INTERP' "$output_root/share/$evidence_name.elf-dynamic" "$output_root/share/$evidence_name.elf-program"; then fail "Linux engine package contains a dynamically loaded executable"; fi
    chmod 0444 "$output_root/share/$evidence_name.elf-header" "$output_root/share/$evidence_name.elf-dynamic" "$output_root/share/$evidence_name.elf-program"
    grep -Eq "Machine:[[:space:]]+$expected_machine$" "$output_root/share/$evidence_name.elf-header" || fail "Linux engine package ELF machine did not match the requested target"
  done
  worker_sha=$(sha256 "$worker")
  guardian_sha=$(sha256 "$guardian")
  install -m 0444 "$worker_root/rclone-LICENSE.txt" "$output_root/share/"
  cp -R "$notices" "$output_root/share/notices"
  find "$output_root/share/notices" -type d -exec chmod 0555 {} +
  find "$output_root/share/notices" -type f -exec chmod 0444 {} +
  install -m 0444 "$notices/THIRD-PARTY-NOTICES.txt" "$output_root/share/"
  install -d -m 0755 "$output_root/share/evidence"
  install -m 0444 "$worker_root/go-target-deps.ndjson" "$worker_root/go-version.txt" "$worker_root/target-license-inventory.json" "$output_root/share/evidence/"
  chmod 0555 "$output_root/share/evidence"
  notice_manifest_sha=$(sha256 "$notices/manifest.json")
  combined_notice_sha=$(sha256 "$notices/THIRD-PARTY-NOTICES.txt")
  combined_notice_size=$(size_of "$notices/THIRD-PARTY-NOTICES.txt")
  test "$(grep -Fc "\"inventorySha256\": \"$inventory_sha\"" "$notices/manifest.json")" -eq 1 || fail "Target notice manifest does not bind the retained inventory"
  cat > "$output_root/share/PROVENANCE.txt" <<EOF
Covalent maintained Linux folder engine
rclone version: $ENGINE_VERSION
Upstream commit: $ENGINE_COMMIT
Restricted module: github.com/thekozugroup/Covalent/packaging/rclone
Build toolchain: Go 1.26.7, CGO_ENABLED=0
Guardian source SHA-256: $GUARDIAN_SOURCE_SHA256
Target architecture: $runtime_arch
Upstream license: MIT
Upstream source: https://github.com/rclone/rclone/tree/$ENGINE_COMMIT
Target notice manifest SHA-256: $notice_manifest_sha
Combined target notices SHA-256: $combined_notice_sha
Target go-list graph SHA-256: $graph_sha
Target binary build metadata SHA-256: $go_version_sha
Target license inventory SHA-256: $inventory_sha
Notice classification and release approval: still required
EOF
  chmod 0444 "$output_root/share/PROVENANCE.txt"
  cat > "$output_root/share/manifest.json" <<EOF
{"schema":1,"engine":{"name":"rclone","version":"$ENGINE_VERSION","commit":"$ENGINE_COMMIT","sourceState":"upstream-unmodified","goVersion":"go1.26.7"},"guardian":{"sourceSha256":"$GUARDIAN_SOURCE_SHA256"},"notices":{"licenseSha256":"$RCLONE_LICENSE_SHA256","targetManifestSha256":"$notice_manifest_sha","combinedSha256":"$combined_notice_sha","combinedBytes":$combined_notice_size,"thirdPartyStatus":"target-texts-collected-review-required"},"targetEvidence":{"goListSha256":"$graph_sha","goListBytes":$graph_size,"goVersionSha256":"$go_version_sha","goVersionBytes":$go_version_size,"licenseInventorySha256":"$inventory_sha","licenseInventoryBytes":$inventory_size},"architecture":"$runtime_arch","executables":{"guardian":{"sha256":"$guardian_sha","bytes":$guardian_size},"worker":{"sha256":"$worker_sha","bytes":$worker_size}}}
EOF
  chmod 0444 "$output_root/share/manifest.json"
  test "$(size_of "$output_root/share/manifest.json")" -le 8192
}

case "${1:-}" in
  worker) shift; build_worker "$@" ;;
  package) shift; package_engine "$@" ;;
  *) echo "usage: $0 worker|package ..." >&2; exit 64 ;;
esac
