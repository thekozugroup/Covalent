#!/bin/sh
# Build the exact Linux folder worker and package it with the reviewed guardian.
set -eu

ENGINE_VERSION=v2.1.3
ENGINE_COMMIT=946e2b83a1f6c6ae119427c09e0a5802940b82ff
SOURCE_ARCHIVE_SHA256=dbcc9498602286a843f29a7104833bd1422082999aa51ff92eef493172d47959
GUARDIAN_SOURCE_SHA256=c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579
LICENSE_SHA256=3f3d9e0024b1921b067d6f7f88deb4a60cbe7a78e76c64e3f1d7fc3b779b9d04
AUTHORS_SHA256=5a0044d13ddf6f013bdd5c2bc419bf45d6123c356567510237e82f304d113d48
MIN_WORKER_BYTES=$((16 * 1024 * 1024))
MAX_WORKER_BYTES=$((48 * 1024 * 1024))
MIN_GUARDIAN_BYTES=$((8 * 1024))
MAX_GUARDIAN_BYTES=$((128 * 1024))

sha256() {
  sha256sum "$1" | awk '{print $1}'
}

size_of() {
  wc -c < "$1" | tr -d '[:space:]'
}

require_hash() {
  actual=$(sha256 "$1")
  test "$actual" = "$2" || {
    echo "Maintained engine source evidence failed its pinned digest" >&2
    exit 1
  }
}

architecture() {
  case "$1" in
    arm64) printf '%s\n' aarch64 ;;
    amd64) printf '%s\n' x86_64 ;;
    *) echo "Unsupported Linux engine architecture: $1" >&2; exit 64 ;;
  esac
}

build_worker() {
  test "$#" -eq 3 || { echo "usage: $0 worker SOURCE OUTPUT TARGETARCH" >&2; exit 64; }
  source_dir=$1
  output_root=$2
  target_arch=$3
  runtime_arch=$(architecture "$target_arch")
  test "$(uname -s)" = Linux && test "$(uname -m)" = "$runtime_arch" || {
    echo "Worker must be built in its exact target-platform container" >&2
    exit 69
  }
  test "$(go version)" = "go version go1.26.7 linux/$target_arch" || {
    echo "Linux engine build requires exact Go 1.26.7 for $target_arch" >&2
    exit 69
  }
  test -f "$source_dir/build.go" && test -f "$source_dir/go.mod" || {
    echo "Pinned Syncthing source export is incomplete" >&2
    exit 66
  }
  require_hash "$source_dir/LICENSE" "$LICENSE_SHA256"
  require_hash "$source_dir/AUTHORS" "$AUTHORS_SHA256"
  go_mod_before=$(sha256 "$source_dir/go.mod")
  go_sum_before=$(sha256 "$source_dir/go.sum")
  mkdir -p "$output_root"
  output="$output_root/covalent-syncthing"
  (
    cd "$source_dir"
    env \
      BUILD_HOST=covalent-linux-release \
      BUILD_USER=covalent-linux-release \
      CGO_ENABLED=0 \
      GOFLAGS=-mod=readonly \
      GOOS=linux \
      GOARCH="$target_arch" \
      GOTOOLCHAIN=local \
      SOURCE_DATE_EPOCH=1785792965 \
      go run build.go \
        -goos linux \
        -goarch "$target_arch" \
        -no-upgrade \
        -version "$ENGINE_VERSION" \
        -build-out "$output" \
        build syncthing
  )
  test "$go_mod_before" = "$(sha256 "$source_dir/go.mod")" && \
    test "$go_sum_before" = "$(sha256 "$source_dir/go.sum")" || {
    echo "Syncthing build changed its pinned module graph" >&2
    exit 1
  }
  worker_size=$(size_of "$output")
  test "$worker_size" -ge "$MIN_WORKER_BYTES" && \
    test "$worker_size" -le "$MAX_WORKER_BYTES" || {
    echo "Linux engine worker size is outside its reviewed bound" >&2
    exit 1
  }
  go version -m "$output" > "$output_root/go-version.txt"
  grep -Fq 'go1.26.7' "$output_root/go-version.txt"
  grep -Eq '^[[:space:]]*path[[:space:]]+github\.com/syncthing/syncthing/cmd/syncthing$' \
    "$output_root/go-version.txt"
  "$output" version | grep -Fq "syncthing $ENGINE_VERSION"
  install -m 0444 "$source_dir/LICENSE" "$output_root/Syncthing-LICENSE.txt"
  install -m 0444 "$source_dir/AUTHORS" "$output_root/Syncthing-AUTHORS.txt"
  chmod 0555 "$output"
}

package_engine() {
  test "$#" -eq 4 || {
    echo "usage: $0 package WORKER-OUTPUT GUARDIAN-SOURCE OUTPUT TARGETARCH" >&2
    exit 64
  }
  worker_root=$1
  guardian_source=$2
  output_root=$3
  target_arch=$4
  runtime_arch=$(architecture "$target_arch")
  case "$target_arch" in
    arm64) expected_machine=AArch64 ;;
    amd64) expected_machine='Advanced Micro Devices X86-64' ;;
  esac
  test "$(uname -s)" = Linux && test "$(uname -m)" = "$runtime_arch" || {
    echo "Engine package must be assembled in its exact target-platform container" >&2
    exit 69
  }
  require_hash "$guardian_source" "$GUARDIAN_SOURCE_SHA256"
  require_hash "$worker_root/Syncthing-LICENSE.txt" "$LICENSE_SHA256"
  require_hash "$worker_root/Syncthing-AUTHORS.txt" "$AUTHORS_SHA256"
  install -d -m 0755 "$output_root/bin" "$output_root/share"
  worker="$output_root/bin/covalent-syncthing"
  guardian="$output_root/bin/covalent-engine-guardian"
  install -m 0555 "$worker_root/covalent-syncthing" "$worker"
  cc -std=c11 -Wall -Wextra -Werror -Os \
    -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fPIE -static-pie \
    -Wl,-s,-z,relro,-z,now,-z,noexecstack \
    "$guardian_source" -o "$guardian"
  chmod 0555 "$guardian"

  worker_size=$(size_of "$worker")
  guardian_size=$(size_of "$guardian")
  printf 'Linux engine package bytes: worker=%s guardian=%s\n' "$worker_size" "$guardian_size"
  test "$worker_size" -ge "$MIN_WORKER_BYTES" && \
    test "$worker_size" -le "$MAX_WORKER_BYTES" || {
    echo "Linux worker exceeds its reviewed package size range" >&2
    exit 1
  }
  test "$guardian_size" -ge "$MIN_GUARDIAN_BYTES" && \
    test "$guardian_size" -le "$MAX_GUARDIAN_BYTES" || {
    echo "Linux guardian exceeds its reviewed package size range" >&2
    exit 1
  }
  readelf=$(cc -print-prog-name=readelf)
  command -v "$readelf" >/dev/null 2>&1 || {
    echo "The pinned Linux builder does not provide readelf" >&2
    exit 69
  }
  for executable in "$worker" "$guardian"; do
    evidence_name=$(basename "$executable")
    elf_header="$output_root/share/$evidence_name.elf-header"
    elf_dynamic="$output_root/share/$evidence_name.elf-dynamic"
    elf_program="$output_root/share/$evidence_name.elf-program"
    "$readelf" -hW "$executable" > "$elf_header"
    "$readelf" -dW "$executable" > "$elf_dynamic"
    "$readelf" -lW "$executable" > "$elf_program"
    if grep -Eq 'NEEDED|INTERP' "$elf_dynamic" "$elf_program"; then
      echo "Linux engine package contains a dynamically loaded executable" >&2
      exit 1
    fi
    chmod 0444 "$elf_header" "$elf_dynamic" "$elf_program"
  done
  for executable in covalent-syncthing covalent-engine-guardian; do
    grep -Eq "Machine:[[:space:]]+$expected_machine$" "$output_root/share/$executable.elf-header" || {
      echo "Linux engine package ELF machine did not match the requested target" >&2
      exit 1
    }
  done

  worker_sha=$(sha256 "$worker")
  guardian_sha=$(sha256 "$guardian")
  install -m 0444 "$worker_root/Syncthing-LICENSE.txt" "$output_root/share/"
  install -m 0444 "$worker_root/Syncthing-AUTHORS.txt" "$output_root/share/"
  cat > "$output_root/share/PROVENANCE.txt" <<EOF
Covalent maintained Linux folder engine
Syncthing version: $ENGINE_VERSION
Upstream commit: $ENGINE_COMMIT
Pinned source archive SHA-256: $SOURCE_ARCHIVE_SHA256
Build toolchain: Go 1.26.7, CGO_ENABLED=0
Guardian source SHA-256: $GUARDIAN_SOURCE_SHA256
Target architecture: $runtime_arch
Upstream license: Mozilla Public License 2.0
Corresponding source: https://github.com/syncthing/syncthing/tree/$ENGINE_COMMIT
Third-party target license aggregation: pending fail-closed release inventory
EOF
  chmod 0444 "$output_root/share/PROVENANCE.txt"
  cat > "$output_root/share/manifest.json" <<EOF
{"schema":1,"engine":{"name":"Syncthing","version":"$ENGINE_VERSION","commit":"$ENGINE_COMMIT","sourceArchiveSha256":"$SOURCE_ARCHIVE_SHA256","goVersion":"go1.26.7"},"guardian":{"sourceSha256":"$GUARDIAN_SOURCE_SHA256"},"notices":{"licenseSha256":"$LICENSE_SHA256","authorsSha256":"$AUTHORS_SHA256","thirdPartyStatus":"pending-fail-closed-target-inventory"},"architecture":"$runtime_arch","executables":{"guardian":{"sha256":"$guardian_sha","bytes":$guardian_size},"worker":{"sha256":"$worker_sha","bytes":$worker_size}}}
EOF
  chmod 0444 "$output_root/share/manifest.json"
  test "$(size_of "$output_root/share/manifest.json")" -le 8192
}

case "${1:-}" in
  worker)
    shift
    build_worker "$@"
    ;;
  package)
    shift
    package_engine "$@"
    ;;
  *)
    echo "usage: $0 worker|package ..." >&2
    exit 64
    ;;
esac
