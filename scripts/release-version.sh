#!/bin/sh
# Single source of truth for the Covalent release version.
#
#   scripts/release-version.sh check        verify every version file agrees (default)
#   scripts/release-version.sh print        print the workspace version of record
#   scripts/release-version.sh set X.Y.Z    rewrite every version file to X.Y.Z
#
# The Cargo workspace version is the version of record. Every other surface is
# derived from it, including the Android versionCode and the Apple
# CFBundleVersion, which are both the monotonic integer
# major * 1000000 + minor * 1000 + patch.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

cargo_manifest="$repo_root/Cargo.toml"
android_gradle="$repo_root/apps/android/app/build.gradle.kts"
apple_project="$repo_root/apps/apple/Project.yml"
workspace_manifests="$repo_root"/crates/*/Cargo.toml

fail() {
  echo "$1" >&2
  exit 1
}

# Case-sensitive filesystems are unforgiving here: these paths must match the
# tracked names exactly, or every version reads as empty and looks like drift.
for required_file in "$cargo_manifest" "$android_gradle" "$apple_project"; do
  [ -f "$required_file" ] || fail "version file is missing: $required_file"
done

require_semver() {
  case "$1" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "version must be MAJOR.MINOR.PATCH: $1" ;;
  esac
  printf '%s' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' ||
    fail "version must be MAJOR.MINOR.PATCH: $1"
}

build_number() {
  printf '%s' "$1" | awk -F. '{ printf "%d", $1 * 1000000 + $2 * 1000 + $3 }'
}

read_cargo_version() {
  awk -F'"' '/^version = "/ { print $2; exit }' "$cargo_manifest"
}

read_android_version_name() {
  awk -F'"' '/versionName = "/ { print $2; exit }' "$android_gradle"
}

read_android_version_code() {
  awk '/versionCode = / { print $3; exit }' "$android_gradle"
}

# The Apple Info.plists are XcodeGen output and are gitignored, so project.yml
# holds the Apple version of record.
read_apple_marketing_version() {
  awk -F'"' '/^ *MARKETING_VERSION: / { print $2; exit }' "$apple_project"
}

read_apple_project_version() {
  awk -F'"' '/^ *CURRENT_PROJECT_VERSION: / { print $2; exit }' "$apple_project"
}

check_workspace_dependency_versions() {
  expected=$1
  status=0
  for manifest in $workspace_manifests; do
    while IFS= read -r line; do
      actual=$(printf '%s\n' "$line" | sed -n 's/.*version = "\([0-9][0-9.]*\)".*path = "\.\.\/covalent-[^"]*".*/\1/p')
      [ -n "$actual" ] || continue
      if [ "$actual" != "$expected" ]; then
        echo "version drift: $manifest has internal Covalent dependency $actual, expected $expected" >&2
        status=1
      fi
    done < "$manifest"
  done
  return "$status"
}

rewrite_workspace_dependency_versions() {
  version=$1
  for manifest in $workspace_manifests; do
    tmp="$manifest.release-version.tmp"
    sed \
      "/path = \"\.\.\/covalent-/ s/version = \"[0-9][0-9.]*\"/version = \"$version\"/" \
      "$manifest" > "$tmp"
    mv "$tmp" "$manifest"
  done
}

rewrite() {
  file=$1
  script=$2
  tmp="$file.release-version.tmp"
  sed "$script" "$file" > "$tmp"
  mv "$tmp" "$file"
}

do_check() {
  version=$(read_cargo_version)
  [ -n "$version" ] || fail "could not read the workspace version from $cargo_manifest"
  require_semver "$version"
  expected_build=$(build_number "$version")

  status=0
  assert() {
    label=$1
    actual=$2
    expected=$3
    if [ "$actual" != "$expected" ]; then
      echo "version drift: $label is '$actual', expected '$expected'" >&2
      status=1
    fi
  }

  assert "apps/android/app/build.gradle.kts versionName" "$(read_android_version_name)" "$version"
  assert "apps/android/app/build.gradle.kts versionCode" "$(read_android_version_code)" "$expected_build"
  assert "apps/apple/Project.yml MARKETING_VERSION" "$(read_apple_marketing_version)" "$version"
  assert "apps/apple/Project.yml CURRENT_PROJECT_VERSION" "$(read_apple_project_version)" "$expected_build"
  if ! check_workspace_dependency_versions "$version"; then
    status=1
  fi
  if [ "$status" -ne 0 ]; then
    echo "run: scripts/release-version.sh set $version" >&2
    exit 1
  fi
  echo "release version $version (build $expected_build): every surface agrees"
}

do_set() {
  version=$1
  require_semver "$version"
  build=$(build_number "$version")

  rewrite "$cargo_manifest" "1,/^version = \"/ s/^version = \".*\"/version = \"$version\"/"
  rewrite "$android_gradle" "s/versionName = \".*\"/versionName = \"$version\"/; s/versionCode = .*/versionCode = $build/"
  rewrite "$apple_project" "s/^\\( *MARKETING_VERSION: \\).*/\\1\"$version\"/; s/^\\( *CURRENT_PROJECT_VERSION: \\).*/\\1\"$build\"/"
  rewrite_workspace_dependency_versions "$version"
  do_check
  echo "remember to run 'cargo update --workspace' so Cargo.lock records $version"
}

command=${1:-check}
case "$command" in
  check) do_check ;;
  print) read_cargo_version ;;
  set)
    [ "$#" -eq 2 ] || fail "usage: scripts/release-version.sh set MAJOR.MINOR.PATCH"
    do_set "$2"
    ;;
  *) fail "usage: scripts/release-version.sh [check|print|set MAJOR.MINOR.PATCH]" ;;
esac
