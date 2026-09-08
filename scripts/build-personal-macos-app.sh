#!/bin/sh
# Build, package, and verify the personal-use Apple Silicon app. This script
# creates artifacts only; it never installs or replaces an application.
set -eu

if [ "$#" -ne 0 ]; then
  echo "usage: $0" >&2
  exit 64
fi

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

if [ "$(uname -s)" != Darwin ] || [ "$(uname -m)" != arm64 ]; then
  fail "The personal macOS app can be built only on an Apple Silicon Mac."
fi

"$repo_root/scripts/release-version.sh" check >/dev/null
version=$("$repo_root/scripts/release-version.sh" print)
case "$version" in
  [0-9]*.[0-9]*.[0-9]*) ;;
  *) fail "Could not read a valid Covalent version." ;;
esac

output_dir="$repo_root/artifacts/install"
archive_name="Covalent-v${version}-macOS-arm64-personal.zip"
checksum_name="${archive_name}.sha256"
archive="$output_dir/$archive_name"
checksum="$output_dir/$checksum_name"

if ! git -C "$repo_root" check-ignore -q "artifacts/install/.ignore-check"; then
  fail "artifacts/install is not ignored by Git; refusing to create install artifacts."
fi

for directory in "$repo_root/artifacts" "$output_dir"; do
  if [ -L "$directory" ]; then
    fail "Refusing a symlinked artifact directory: $directory"
  fi
  if [ -e "$directory" ] && [ ! -d "$directory" ]; then
    fail "Artifact path is not a directory: $directory"
  fi
done

for path in "$archive" "$checksum"; do
  if [ -e "$path" ] || [ -L "$path" ]; then
    fail "Refusing to overwrite existing output: $path"
  fi
done

mkdir -p "$output_dir"
umask 077
build_dir=$(mktemp -d "$output_dir/.personal-macos-build.XXXXXX")
case "$build_dir" in
  "$output_dir"/.personal-macos-build.*) ;;
  *) fail "Could not create a private build directory." ;;
esac

cleanup() {
  rm -rf -- "$build_dir"
}
trap cleanup EXIT HUP INT TERM

printf '%s\n' "Preparing pinned build tools..."
xcodegen_bin=$("$repo_root/scripts/install-xcodegen.sh" "$build_dir/xcodegen")
PATH="$xcodegen_bin:$PATH"
export PATH
"$repo_root/scripts/setup-doctor.sh" macos

if ! rustup target list --installed | grep -Fx aarch64-apple-darwin >/dev/null; then
  fail "Rust target aarch64-apple-darwin is missing. Run: rustup target add aarch64-apple-darwin"
fi
if [ ! -s "$repo_root/apps/apple/Package.resolved" ]; then
  fail "Locked Swift package data is missing: apps/apple/Package.resolved"
fi

printf '%s\n' "Preparing the pinned folder-sync source and private Go toolchain..."
syncthing_source="$build_dir/syncthing"
syncthing_revision=946e2b83a1f6c6ae119427c09e0a5802940b82ff
git init --quiet "$syncthing_source"
git -C "$syncthing_source" remote add origin https://github.com/syncthing/syncthing.git
git -C "$syncthing_source" fetch --quiet --depth=1 origin "$syncthing_revision"
git -C "$syncthing_source" checkout --quiet --detach FETCH_HEAD
if [ "$(git -C "$syncthing_source" rev-parse HEAD)" != "$syncthing_revision" ]; then
  fail "The folder-sync source does not match its pinned revision."
fi

go_archive="$build_dir/go1.26.7.darwin-arm64.tar.gz"
curl --fail --location --proto '=https' --proto-redir '=https' --tlsv1.2 \
  --connect-timeout 15 --max-time 300 --max-filesize 67108864 \
  --output "$go_archive" https://go.dev/dl/go1.26.7.darwin-arm64.tar.gz
if [ "$(shasum -a 256 "$go_archive" | awk '{print $1}')" != \
  020a1e8224811be75163e920bc77e0926a1390a6aeea19bdcf23f74b9d749f6d ] || \
  [ "$(wc -c < "$go_archive" | tr -d '[:space:]')" -ne 64772572 ]; then
  fail "The private Go toolchain did not match its pinned checksum and size."
fi
SYNCTHING_SOURCE_DIR="$syncthing_source"
COVALENT_GO_ARCHIVE="$go_archive"
export SYNCTHING_SOURCE_DIR COVALENT_GO_ARCHIVE

archive_path="$build_dir/CovalentMac.xcarchive"
swift_packages="$build_dir/swift-packages"
derived_data="$build_dir/DerivedData"

printf '%s\n' "Building Covalent for Apple Silicon..."
(
  cd "$repo_root/apps/apple"
  xcodegen generate --quiet
  xcodebuild \
    -resolvePackageDependencies \
    -project Covalent.xcodeproj \
    -scheme CovalentMac \
    -clonedSourcePackagesDirPath "$swift_packages" \
    -disableAutomaticPackageResolution \
    -onlyUsePackageVersionsFromResolvedFile \
    -skipPackageUpdates \
    -packageFingerprintPolicy strict \
    -packageSigningEntityPolicy strict
  xcodebuild \
    -project Covalent.xcodeproj \
    -scheme CovalentMac \
    -configuration Release \
    -destination 'generic/platform=macOS' \
    -archivePath "$archive_path" \
    -derivedDataPath "$derived_data" \
    -clonedSourcePackagesDirPath "$swift_packages" \
    -disableAutomaticPackageResolution \
    -onlyUsePackageVersionsFromResolvedFile \
    -skipPackageUpdates \
    -packageFingerprintPolicy strict \
    -packageSigningEntityPolicy strict \
    ARCHS=arm64 \
    EXCLUDED_ARCHS=x86_64 \
    CODE_SIGN_STYLE=Manual \
    CODE_SIGN_IDENTITY=- \
    DEVELOPMENT_TEAM="" \
    PROVISIONING_PROFILE_SPECIFIER="" \
    OTHER_CODE_SIGN_FLAGS="" \
    archive
)

app="$archive_path/Products/Applications/Covalent.app"
"$repo_root/scripts/verify-apple-silicon-bundle.sh" "$app"
if ! codesign -d --verbose=4 "$app" 2>&1 | grep -Fq 'Signature=adhoc'; then
  fail "The built app does not have the required ad-hoc signature."
fi

staged_archive="$build_dir/$archive_name"
staged_checksum="$build_dir/$checksum_name"
ditto -c -k --sequesterRsrc --keepParent "$app" "$staged_archive"
(
  cd "$build_dir"
  shasum -a 256 "$archive_name" > "$checksum_name"
  shasum -a 256 -c "$checksum_name"
)

unpacked_dir="$build_dir/unpacked"
mkdir "$unpacked_dir"
ditto -x -k "$staged_archive" "$unpacked_dir"
unpacked_app="$unpacked_dir/Covalent.app"
"$repo_root/scripts/verify-apple-silicon-bundle.sh" "$unpacked_app"
if ! codesign -d --verbose=4 "$unpacked_app" 2>&1 | grep -Fq 'Signature=adhoc'; then
  fail "The packaged app does not retain the required ad-hoc signature."
fi

# Check again after the long build. BSD mv -n leaves the staged file in place
# when another process created the destination, which makes the collision
# detectable without replacing either output.
for path in "$archive" "$checksum"; do
  if [ -e "$path" ] || [ -L "$path" ]; then
    fail "Refusing to overwrite output created during the build: $path"
  fi
done

mv -n "$staged_archive" "$archive"
if [ -e "$staged_archive" ]; then
  fail "Could not publish without overwriting: $archive"
fi
mv -n "$staged_checksum" "$checksum"
if [ -e "$staged_checksum" ]; then
  fail "Could not publish without overwriting: $checksum"
fi

(
  cd "$output_dir"
  shasum -a 256 -c "$checksum_name"
)

printf '\nPersonal macOS app ready:\n  %s\n  %s\n' "$archive" "$checksum"
printf '%s\n' "Nothing was installed or replaced. Follow docs/platform/macos.md to install it."
