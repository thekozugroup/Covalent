#!/bin/sh
set -eu

: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${GH_TOKEN:?GH_TOKEN is required}"

verification=$(gh api "repos/${GITHUB_REPOSITORY}/commits/${GITHUB_SHA}" --jq '.commit.verification')
if ! printf '%s\n' "${verification}" | jq -e '.verified == true' >/dev/null; then
  reason=$(printf '%s\n' "${verification}" | jq -r '.reason // "unknown"')
  echo "release commit ${GITHUB_SHA} lacks a verified signature (${reason})." >&2
  echo "GitHub reports verification.verified=false for this commit, so no release may be published from it." >&2
  echo "Register an SSH signing key once, then re-tag. Exact one-time commands:" >&2
  echo "  gh auth refresh -h github.com -s admin:ssh_signing_key" >&2
  echo "  ssh-keygen -t ed25519 -C \"\${USER}\" -f ~/.ssh/covalent_signing" >&2
  echo "  gh ssh-key add ~/.ssh/covalent_signing.pub --type signing --title 'Covalent release signing'" >&2
  echo "  git config --global gpg.format ssh" >&2
  echo "  git config --global user.signingkey ~/.ssh/covalent_signing.pub" >&2
  echo "  git config --global commit.gpgsign true" >&2
  echo "  git config --global tag.gpgsign true" >&2
  echo "See docs/release/commit-signing.md. Do not remove this gate." >&2
  exit 1
fi
echo "release commit ${GITHUB_SHA} has a verified signature."
