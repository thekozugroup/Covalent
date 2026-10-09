#!/usr/bin/env bash
set -euo pipefail
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
cat > "$fixture/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
case "$2" in
  */git/ref/tags/release-tools-v0.2.12-container-2)
    printf '{"object":{"type":"%s","sha":"fixture-tag"}}\n' "${TEST_REF_TYPE:-tag}" ;;
  */git/tags/fixture-tag)
    printf '{"object":{"sha":"%s"},"verification":{"verified":%s}}\n' "${TEST_TARGET:-fixture-repair-commit}" "${TEST_TAG_VERIFIED:-true}" ;;
  */commits/fixture-repair-commit)
    printf '%s\n' "${TEST_COMMIT_VERIFIED:-true}" ;;
  *) exit 9 ;;
esac
MOCK
chmod +x "$fixture/gh"
export PATH="$fixture:$PATH"
export GH_TOKEN=fixture-token
export GITHUB_REPOSITORY=thekozugroup/Covalent
export GITHUB_SHA=fixture-repair-commit
export GITHUB_EVENT_NAME=workflow_dispatch
export GITHUB_REF_TYPE=tag
export GITHUB_REF=refs/tags/release-tools-v0.2.12-container-2
source_sha=7bb83c27ed56d2b78517845db7bdf1757b80fbe1
gate="$repo_root/scripts/verify-container-build-repair.sh"
expect_failure() {
  if "$@" > "$fixture/result" 2>&1; then
    echo "invalid repair unexpectedly accepted: $*" >&2
    exit 1
  fi
}
"$gate" v0.2.12 "$source_sha"
expect_failure "$gate" v0.2.13 "$source_sha"
expect_failure "$gate" v0.2.12 0000000000000000000000000000000000000000
expect_failure env GITHUB_EVENT_NAME=push "$gate" v0.2.12 "$source_sha"
expect_failure env GITHUB_REF_TYPE=branch "$gate" v0.2.12 "$source_sha"
expect_failure env GITHUB_REF=refs/heads/main "$gate" v0.2.12 "$source_sha"
expect_failure env GITHUB_REF=refs/tags/release-tools-v0.2.13-container "$gate" v0.2.12 "$source_sha"
expect_failure env GITHUB_REF=refs/tags/release-tools-v0.2.12-container "$gate" v0.2.12 "$source_sha"
expect_failure env TEST_REF_TYPE=commit "$gate" v0.2.12 "$source_sha"
expect_failure env TEST_TARGET=another-commit "$gate" v0.2.12 "$source_sha"
expect_failure env TEST_TAG_VERIFIED=false "$gate" v0.2.12 "$source_sha"
expect_failure env TEST_COMMIT_VERIFIED=false "$gate" v0.2.12 "$source_sha"
echo "Container build repair: signed exact source accepted; 11 invalid identities rejected."
