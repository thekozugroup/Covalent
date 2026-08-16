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

arm64_binary=""
x86_64_binary=""
for build_arch in $build_arches; do
  case "$build_arch" in
    arm64)
      rust_target=aarch64-apple-darwin
      if [ -n "$arm64_binary" ]; then
        continue
      fi
      ;;
    x86_64)
      rust_target=x86_64-apple-darwin
      if [ -n "$x86_64_binary" ]; then
        continue
      fi
      ;;
    *)
      printf '%s\n' "Unsupported macOS build architecture: $build_arch" >&2
      exit 1
      ;;
  esac

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
  case "$build_arch" in
    arm64) arm64_binary=$source_binary ;;
    x86_64) x86_64_binary=$source_binary ;;
  esac
done

destination_directory="$TARGET_BUILD_DIR/$EXECUTABLE_FOLDER_PATH"
destination_binary="$destination_directory/covalent-node"
mkdir -p "$destination_directory"
if [ -n "$arm64_binary" ] && [ -n "$x86_64_binary" ]; then
  xcrun lipo -create "$arm64_binary" "$x86_64_binary" -output "$destination_binary"
elif [ -n "$arm64_binary" ]; then
  ditto "$arm64_binary" "$destination_binary"
elif [ -n "$x86_64_binary" ]; then
  ditto "$x86_64_binary" "$destination_binary"
else
  printf '%s\n' "No supported macOS helper architecture was requested." >&2
  exit 1
fi
chmod 755 "$destination_binary"

if [ "${CODE_SIGNING_ALLOWED:-YES}" = "YES" ]; then
  identity=${EXPANDED_CODE_SIGN_IDENTITY:--}
  codesign \
    --force \
    --sign "$identity" \
    --entitlements "$apple_dir/Config/CovalentNode.entitlements" \
    --timestamp=none \
    "$destination_binary"
fi
