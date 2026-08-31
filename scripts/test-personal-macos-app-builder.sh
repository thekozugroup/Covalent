#!/bin/sh
# Fast contract test for the personal macOS packaging entry point. The real
# bundle verifier remains the authority for an actual built app.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
builder="$repo_root/scripts/build-personal-macos-app.sh"

fail() {
  printf '%s\n' "$1" >&2
  exit 1
}

test -x "$builder" || fail "personal macOS builder is not executable"
sh -n "$builder"
git -C "$repo_root" check-ignore -q artifacts/install/.ignore-check ||
  fail "artifacts/install must stay ignored"

# These are literal source contracts, not expressions to expand in this test.
# shellcheck disable=SC2016
for contract in \
  '"$(uname -s)" != Darwin' \
  '"$(uname -m)" != arm64' \
  'scripts/install-xcodegen.sh' \
  'scripts/setup-doctor.sh' \
  'scripts/release-version.sh' \
  'xcodegen generate --quiet' \
  '-disableAutomaticPackageResolution' \
  '-onlyUsePackageVersionsFromResolvedFile' \
  'ARCHS=arm64' \
  'EXCLUDED_ARCHS=x86_64' \
  'CODE_SIGN_IDENTITY=-' \
  'scripts/verify-apple-silicon-bundle.sh' \
  'Signature=adhoc' \
  'artifacts/install' \
  'shasum -a 256 -c' \
  'ditto -x -k' \
  'mv -n'
do
  grep -Fq -- "$contract" "$builder" || fail "missing builder contract: $contract"
done

if grep -Eq 'notarytool|altool|stapler|security[[:space:]]+import|open[[:space:]]+-[Ra]' "$builder" ||
  grep -Eq '(^|[[:space:]])(cp|mv|ditto)[[:space:]].*/Applications(/|[[:space:]]|$)' "$builder"; then
  fail "personal builder must not sign for distribution, notarize, or install"
fi

# shellcheck disable=SC2016
grep -Fq 'if [ -e "$path" ] || [ -L "$path" ]; then' "$builder" ||
  fail "builder must refuse existing files and symlinks"
grep -Fq './scripts/build-personal-macos-app.sh' "$repo_root/docs/platform/macos.md" ||
  fail "macOS guide must use the one-command builder"
grep -Fq './scripts/build-personal-macos-app.sh' "$repo_root/apps/apple/README.md" ||
  fail "Apple developer README must use the one-command builder"

echo "Personal macOS builder contract: ok"
