#!/bin/sh
# Exercise version failures in an isolated fixture, without editing the checkout.
set -eu
repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/covalent-version-test.XXXXXX")
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
mkdir -p "$fixture/scripts" "$fixture/apps/android/app" "$fixture/apps/apple" "$fixture/crates/covalent-node"
cp "$repo_root/scripts/release-version.sh" "$fixture/scripts/"
cp "$repo_root/Cargo.toml" "$fixture/"
cp "$repo_root/apps/android/app/build.gradle.kts" "$fixture/apps/android/app/"
cp "$repo_root/apps/apple/Project.yml" "$fixture/apps/apple/"
cp "$repo_root/crates/covalent-node/Cargo.toml" "$fixture/crates/covalent-node/"
check="$fixture/scripts/release-version.sh"
"$check" check >/dev/null
expect_rejection() {
  if "$check" check >"$fixture/result" 2>&1; then
    echo "version gate accepted $1" >&2
    exit 1
  fi
  grep -q 'version drift:' "$fixture/result"
  "$check" set 0.2.4 >/dev/null
}
"$check" set 0.2.4 >/dev/null
sed 's/versionName = "0.2.4"/versionName = "0.2.3"/' "$fixture/apps/android/app/build.gradle.kts" >"$fixture/changed"
mv "$fixture/changed" "$fixture/apps/android/app/build.gradle.kts"
expect_rejection 'Android versionName drift'
sed 's/versionCode = 2004/versionCode = 2003/' "$fixture/apps/android/app/build.gradle.kts" >"$fixture/changed"
mv "$fixture/changed" "$fixture/apps/android/app/build.gradle.kts"
expect_rejection 'Android versionCode drift'
sed 's/MARKETING_VERSION: "0.2.4"/MARKETING_VERSION: "0.2.3"/' "$fixture/apps/apple/Project.yml" >"$fixture/changed"
mv "$fixture/changed" "$fixture/apps/apple/Project.yml"
expect_rejection 'Apple marketing version drift'
sed '/path = "..\/covalent-/ s/version = "0.2.4"/version = "0.2.3"/' "$fixture/crates/covalent-node/Cargo.toml" >"$fixture/changed"
mv "$fixture/changed" "$fixture/crates/covalent-node/Cargo.toml"
expect_rejection 'workspace dependency drift'
"$check" check >/dev/null
echo 'Release version failure regression: ok'
