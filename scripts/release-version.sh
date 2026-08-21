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
apple_project="$repo_root/apps/apple/project.yml"
unraid_template="$repo_root/packaging/unraid/covalent.xml"

fail() {
  echo "$1" >&2
  exit 1
}

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

read_unraid_version() {
  awk -F'[:<]' '/<Repository>/ { print $3; exit }' "$unraid_template"
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
  assert "apps/apple/project.yml MARKETING_VERSION" "$(read_apple_marketing_version)" "$version"
  assert "apps/apple/project.yml CURRENT_PROJECT_VERSION" "$(read_apple_project_version)" "$expected_build"
  assert "packaging/unraid/covalent.xml Repository tag" "$(read_unraid_version)" "v$version"

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
  rewrite "$unraid_template" "s|<Repository>\(.*\):v[0-9][0-9.]*</Repository>|<Repository>\1:v$version</Repository>|"

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
