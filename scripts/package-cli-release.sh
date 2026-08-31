#!/bin/sh
# Create one self-contained, architecture-checked Covalent CLI release archive.
#
# This intentionally does not build, sign, upload, or publish anything. Keeping
# packaging deterministic and side-effect-free lets the release workflow verify
# every artifact before any release asset is made visible.
set -eu

usage() {
  echo "usage: $0 --binary PATH --platform linux-amd64|linux-arm64|macos-arm64 --version vX.Y.Z --output-dir PATH" >&2
  exit 2
}

binary=""
platform=""
version=""
output_dir=""

while [ "$#" -gt 0 ]; do
  case "$1" in
    --binary) binary=${2:-}; shift 2 ;;
    --platform) platform=${2:-}; shift 2 ;;
    --version) version=${2:-}; shift 2 ;;
    --output-dir) output_dir=${2:-}; shift 2 ;;
    *) usage ;;
  esac
done

[ -n "$binary" ] && [ -n "$platform" ] && [ -n "$version" ] && [ -n "$output_dir" ] || usage
[ -f "$binary" ] || { echo "CLI binary is missing: $binary" >&2; exit 1; }
[ -x "$binary" ] || { echo "CLI binary is not executable: $binary" >&2; exit 1; }

case "$version" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *) echo "release version must be v-prefixed semantic version: $version" >&2; exit 1 ;;
esac

if ! command -v file >/dev/null 2>&1 || ! command -v tar >/dev/null 2>&1; then
  echo "package-cli-release.sh requires file and tar" >&2
  exit 1
fi

binary_description=$(file -b "$binary")
case "$platform" in
  linux-amd64)
    printf '%s\n' "$binary_description" | grep -Eq 'ELF 64-bit.*(x86-64|x86_64)' || {
      echo "expected linux amd64 CLI, got: $binary_description" >&2; exit 1;
    }
    ;;
  linux-arm64)
    printf '%s\n' "$binary_description" | grep -Eq 'ELF 64-bit.*(aarch64|ARM aarch64)' || {
      echo "expected linux arm64 CLI, got: $binary_description" >&2; exit 1;
    }
    ;;
  macos-arm64)
    printf '%s\n' "$binary_description" | grep -Eq 'Mach-O 64-bit executable arm64' || {
      echo "expected Apple Silicon macOS CLI, got: $binary_description" >&2; exit 1;
    }
    if printf '%s\n' "$binary_description" | grep -Eq 'x86_64|universal'; then
      echo "macOS CLI must be arm64-only, got: $binary_description" >&2
      exit 1
    fi
    ;;
  *) usage ;;
esac

max_cli_bytes=$((8 * 1024 * 1024))
binary_bytes=$(wc -c < "$binary" | tr -d '[:space:]')
case "$binary_bytes" in ''|*[!0-9]*) echo "could not measure CLI size" >&2; exit 1 ;; esac
[ "$binary_bytes" -le "$max_cli_bytes" ] || {
  echo "CLI exceeds 8 MiB budget: $binary_bytes bytes" >&2; exit 1;
}

reported_version=$($binary --version)
case "$reported_version" in
  *"${version#v}"*) ;;
  *)
    echo "CLI reports an unexpected version: $reported_version" >&2
    exit 1
    ;;
esac

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
mkdir -p "$output_dir"
stage=$(mktemp -d "${TMPDIR:-/tmp}/covalent-cli-release.XXXXXX")
trap 'rm -rf "$stage"' EXIT INT TERM

archive_base="Covalent-${version}-${platform}"
archive_dir="$stage/$archive_base"
mkdir -p "$archive_dir"
cp "$binary" "$archive_dir/covalent"
cp "$repo_root/LICENSE" "$archive_dir/LICENSE"
cat > "$archive_dir/INSTALL.txt" <<EOF
Covalent CLI ${version} (${platform})

1. Verify this archive with the release SHA256SUMS and its Sigstore bundle.
2. Extract it: tar -xzf ${archive_base}.tar.gz
3. Move the covalent binary to a directory on your PATH, then run:
   covalent --help

Do not use a curl-pipe-shell installer. Full verification instructions:
https://github.com/thekozugroup/Covalent/blob/main/docs/release/cli-install.md
EOF

archive="$output_dir/${archive_base}.tar.gz"
rm -f "$archive"
(cd "$stage" && tar -czf "$archive" "$archive_base")

echo "archive=$archive"
echo "binary_bytes=$binary_bytes"
echo "architecture=$binary_description"
