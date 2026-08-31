#!/bin/sh
# Install the exact XcodeGen release used by Apple release workflows. Homebrew
# formulae are intentionally not used here: a release must not regenerate its
# project from a later mutable package-manager revision.
set -eu

if [ "$#" -ne 1 ]; then
  echo "usage: $0 DESTINATION" >&2
  exit 64
fi

destination=$1
case "$destination" in
  '' | / | . | ..)
    echo "XcodeGen destination must be a new, specific directory" >&2
    exit 64
    ;;
esac

if [ -e "$destination" ]; then
  echo "refusing to reuse an existing XcodeGen destination: $destination" >&2
  exit 73
fi

xcodegen_version=2.46.0
xcodegen_sha256=4d9e34b62172d645eed6457cac13fc222569974098ef4ee9c3368bedf0196806
xcodegen_url="https://github.com/yonaskolb/XcodeGen/releases/download/${xcodegen_version}/xcodegen.zip"

umask 077
xcodegen_tmp_root=${TMPDIR:-/tmp}
xcodegen_tmp_dir=$(mktemp -d "${xcodegen_tmp_root%/}/covalent-xcodegen.XXXXXX")
cleanup() {
  rm -rf -- "$xcodegen_tmp_dir"
}
trap cleanup EXIT HUP INT TERM

xcodegen_archive="$xcodegen_tmp_dir/xcodegen.zip"
curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
  --retry 3 --output "$xcodegen_archive" "$xcodegen_url"
actual_sha256=$(shasum -a 256 "$xcodegen_archive" | awk '{ print $1 }')
if [ "$actual_sha256" != "$xcodegen_sha256" ]; then
  echo "XcodeGen archive checksum mismatch" >&2
  exit 65
fi

mkdir -p "$destination"
unzip -qq "$xcodegen_archive" -d "$destination"
xcodegen_binary="$destination/xcodegen/bin/xcodegen"
if [ ! -x "$xcodegen_binary" ] || ! "$xcodegen_binary" --version | grep -Fx "Version: ${xcodegen_version}" >/dev/null; then
  echo "verified XcodeGen archive did not contain version ${xcodegen_version}" >&2
  exit 65
fi

printf '%s\n' "$destination/xcodegen/bin"
