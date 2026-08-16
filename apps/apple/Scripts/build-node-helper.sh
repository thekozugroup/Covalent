#!/bin/sh
set -eu

if [ -n "${SRCROOT:-}" ]; then
  apple_dir=$SRCROOT
else
  script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
  apple_dir=$(CDPATH= cd -- "$script_dir/.." && pwd)
fi
repo_root=$(CDPATH= cd -- "$apple_dir/../.." && pwd)
configuration=${CONFIGURATION:-Debug}
profile_directory=debug
if [ "$configuration" = "Release" ]; then
  profile_directory=release
fi

# Prefer the repository-selected rustup toolchain over a Homebrew cargo/rustc
# pair, because only rustup can supply the pinned cross-compilation stdlibs.
if command -v rustup >/dev/null 2>&1; then
  rust_toolchain_bin=$(dirname "$(rustup which cargo)")
  PATH="$rust_toolchain_bin:$PATH"
  export PATH
fi

build_arches=${ARCHS:-${CURRENT_ARCH:-}}
if [ -z "$build_arches" ] || [ "$build_arches" = "undefined_arch" ]; then
  build_arches=${NATIVE_ARCH_ACTUAL:-$(uname -m)}
fi

# Covalent for macOS is intentionally Apple Silicon only. Fail closed when
# Xcode requests anything other than the single supported architecture.
if [ "$build_arches" != "arm64" ]; then
  printf '%s\n' "CovalentMac requires exactly ARCHS=arm64; received: $build_arches" >&2
  exit 1
fi

rust_target=aarch64-apple-darwin
if [ "$configuration" = "Release" ]; then
  cargo build \
    --locked \
    --release \
    --manifest-path "$repo_root/Cargo.toml" \
    --package covalent-node \
    --bin covalent-node \
    --target "$rust_target"
else
  cargo build \
    --locked \
    --manifest-path "$repo_root/Cargo.toml" \
    --package covalent-node \
    --bin covalent-node \
    --target "$rust_target"
fi

source_binary="$repo_root/target/$rust_target/$profile_directory/covalent-node"

destination_directory="$TARGET_BUILD_DIR/$EXECUTABLE_FOLDER_PATH"
destination_binary="$destination_directory/covalent-node"
mkdir -p "$destination_directory"
ditto "$source_binary" "$destination_binary"
if [ "$(xcrun lipo -archs "$destination_binary")" != "arm64" ]; then
  printf '%s\n' "Bundled Covalent node is not arm64-only." >&2
  exit 1
fi
chmod 755 "$destination_binary"

if [ "${CODE_SIGNING_ALLOWED:-YES}" = "YES" ]; then
  identity=${EXPANDED_CODE_SIGN_IDENTITY:--}
  if [ "$configuration" = "Release" ] && [ "$identity" != "-" ]; then
    codesign \
      --force \
      --options runtime \
      --sign "$identity" \
      --entitlements "$apple_dir/Config/CovalentNode.entitlements" \
      --timestamp \
      "$destination_binary"
  else
    codesign \
      --force \
      --sign "$identity" \
      --entitlements "$apple_dir/Config/CovalentNode.entitlements" \
      --timestamp=none \
      "$destination_binary"
  fi
fi
