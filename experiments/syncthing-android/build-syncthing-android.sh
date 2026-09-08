#!/bin/sh
set -eu

test "$#" = 2 || {
  echo "usage: $0 /exact/syncthing/checkout /generated/output" >&2
  exit 2
}
source_dir=${1:?Pass the exact Syncthing source checkout}
output_root=${2:?Pass the generated output directory}
proof_root=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
guardian_source="$proof_root/engine-guardian.c"

expected_commit=946e2b83a1f6c6ae119427c09e0a5802940b82ff
expected_go='go version go1.26.7 '
expected_ndk=27.1.12297006
min_bytes=5242880
max_bytes=67108864
guardian_source_sha=c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579
guardian_min_bytes=8192
guardian_max_bytes=262144

test "$(shasum -a 256 "$guardian_source" | awk '{print $1}')" = "$guardian_source_sha" || {
  echo "Reviewed engine guardian source hash does not match" >&2
  exit 1
}

test -d "$source_dir/.git" || {
  echo "Syncthing source must be an exact git checkout" >&2
  exit 1
}
actual_commit=$(git -C "$source_dir" rev-parse HEAD)
test "$actual_commit" = "$expected_commit" || {
  echo "Expected Syncthing $expected_commit, found $actual_commit" >&2
  exit 1
}
test -z "$(git -C "$source_dir" status --porcelain --untracked-files=no)" || {
  echo "Tracked Syncthing source differs from the pinned commit" >&2
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
test -f "$ndk_dir/source.properties" || {
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
mkdir -p "$jni_root/arm64-v8a" "$jni_root/x86_64" "$assets_root"
hash_file="$assets_root/syncthing-sha256.txt"
guardian_hash_file="$assets_root/engine-guardian-sha256.txt"
: > "$hash_file"
: > "$guardian_hash_file"

build_one() {
  abi=$1
  goarch=$2
  compiler=$3
  output="$jni_root/$abi/libsyncthing.so"
  cc="$toolchain/bin/$compiler"
  test -x "$cc" || { echo "Missing NDK compiler: $cc" >&2; exit 1; }

  (
    cd "$source_dir"
    env \
      BUILD_HOST=covalent-engine-proof \
      BUILD_USER=covalent-engine-proof \
      CGO_ENABLED=1 \
      GOFLAGS=-mod=readonly \
      GOTOOLCHAIN=local \
      SOURCE_DATE_EPOCH=1785792965 \
      EXTRA_LDFLAGS='-checklinkname=0 -linkmode=external -extldflags=-Wl,-z,max-page-size=16384' \
      go run build.go \
        -goos android \
        -goarch "$goarch" \
        -cc "$cc" \
        -no-upgrade \
        -version v2.1.3 \
        -build-out "$output" \
        build syncthing
  )

  size=$(wc -c < "$output" | tr -d '[:space:]')
  test "$size" -ge "$min_bytes" && test "$size" -le "$max_bytes" || {
    echo "$abi helper size $size is outside [$min_bytes, $max_bytes]" >&2
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
  "$readelf" -dW "$output" | grep -Eq 'TEXTREL|DT_TEXTREL' && {
    echo "$abi helper contains a text relocation" >&2
    exit 1
  }

  needed=$("$readelf" -dW "$output" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)
  unexpected=$(printf '%s\n' "$needed" | sed '/^$/d' | grep -Ev '^(libc|libdl|liblog|libm)\.so$' || true)
  test -z "$unexpected" || {
    echo "$abi helper has unexpected runtime dependencies: $unexpected" >&2
    exit 1
  }

  hash=$(shasum -a 256 "$output" | awk '{print $1}')
  printf '%s %s %s\n' "$abi" "$hash" "$size" >> "$hash_file"
  test -z "$(git -C "$source_dir" status --porcelain --untracked-files=no)" || {
    echo "Syncthing build modified tracked pinned source" >&2
    exit 1
  }
  echo "  $abi: $size bytes, sha256=$hash"
}

build_guardian() {
  abi=$1
  compiler=$2
  output="$jni_root/$abi/libengineguardian.so"
  cc="$toolchain/bin/$compiler"
  readelf="$toolchain/bin/llvm-readelf"
  test -x "$cc" || { echo "Missing NDK compiler: $cc" >&2; exit 1; }

  "$cc" \
    -std=c11 -Wall -Wextra -Werror -O2 \
    -D_FORTIFY_SOURCE=2 -fstack-protector-strong -fPIE -pie \
    -Wl,-z,relro,-z,now,-z,noexecstack,-z,max-page-size=16384 \
    "$guardian_source" -o "$output"

  size=$(wc -c < "$output" | tr -d '[:space:]')
  test "$size" -ge "$guardian_min_bytes" && test "$size" -le "$guardian_max_bytes" || {
    echo "$abi guardian size $size is outside [$guardian_min_bytes, $guardian_max_bytes]" >&2
    exit 1
  }
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
  "$readelf" -dW "$output" | grep -Eq 'BIND_NOW|FLAGS.*NOW' || {
    echo "$abi guardian is missing immediate relocation binding" >&2
    exit 1
  }
  "$readelf" -dW "$output" | grep -Eq 'TEXTREL|DT_TEXTREL' && {
    echo "$abi guardian contains a text relocation" >&2
    exit 1
  }
  needed=$("$readelf" -dW "$output" | sed -n 's/.*Shared library: \[\([^]]*\)\].*/\1/p' | sort -u)
  unexpected=$(printf '%s\n' "$needed" | sed '/^$/d' | grep -Ev '^libc\.so$' || true)
  test -z "$unexpected" || {
    echo "$abi guardian has unexpected runtime dependencies: $unexpected" >&2
    exit 1
  }

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
