#!/bin/sh
set -eu

: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${GH_TOKEN:?GH_TOKEN is required}"

verification=$(gh api "repos/${GITHUB_REPOSITORY}/commits/${GITHUB_SHA}" --jq '.commit.verification')
if ! printf '%s\n' "${verification}" | jq -e '.verified == true' >/dev/null; then
  reason=$(printf '%s\n' "${verification}" | jq -r '.reason // "unknown"')
  echo "release commit ${GITHUB_SHA} lacks a verified signature (${reason})." >&2
  exit 1
fi
echo "release commit ${GITHUB_SHA} has a verified signature."
