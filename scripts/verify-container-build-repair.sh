#!/usr/bin/env bash
set -euo pipefail

if (( $# != 2 )); then
  echo "usage: verify-container-build-repair.sh <version> <runtime-source-sha>" >&2
  exit 2
fi
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${GH_TOKEN:?GH_TOKEN is required}"

# One source-bound build repair for the Docker Hub outage. This does not
# authorize another runtime, an arbitrary repair ref, or archive substitution.
test "$1" = v0.2.12
test "$2" = 7bb83c27ed56d2b78517845db7bdf1757b80fbe1
test "${GITHUB_EVENT_NAME:-}" = workflow_dispatch
test "${GITHUB_REF_TYPE:-}" = tag
repair_tag=release-tools-v0.2.12-container
test "${GITHUB_REF:-}" = "refs/tags/${repair_tag}"

ref=$(gh api "repos/${GITHUB_REPOSITORY}/git/ref/tags/${repair_tag}")
test "$(jq -r '.object.type // empty' <<<"${ref}")" = tag
tag_sha=$(jq -r '.object.sha // empty' <<<"${ref}")
test -n "${tag_sha}"
tag=$(gh api "repos/${GITHUB_REPOSITORY}/git/tags/${tag_sha}")
test "$(jq -r '.object.sha // empty' <<<"${tag}")" = "${GITHUB_SHA}"
test "$(jq -r '.verification.verified // false' <<<"${tag}")" = true
test "$(gh api "repos/${GITHUB_REPOSITORY}/commits/${GITHUB_SHA}" --jq '.commit.verification.verified')" = true
echo "Verified signed v0.2.12 build repair; runtime source remains $2."
