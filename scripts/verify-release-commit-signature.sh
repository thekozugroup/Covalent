#!/bin/sh
set -eu

: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"
: "${GITHUB_SHA:?GITHUB_SHA is required}"
: "${GH_TOKEN:?GH_TOKEN is required}"
: "${RELEASE_VERSION:?RELEASE_VERSION is required}"

case "${RELEASE_VERSION}" in
  v[0-9]*.[0-9]*.[0-9]*) ;;
  *)
    echo "release tag must be an explicit v-prefixed semantic version: ${RELEASE_VERSION}" >&2
    exit 1
    ;;
esac

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

tag_ref=$(gh api "repos/${GITHUB_REPOSITORY}/git/ref/tags/${RELEASE_VERSION}")
tag_object_type=$(printf '%s\n' "${tag_ref}" | jq -r '.object.type // empty')
tag_object_sha=$(printf '%s\n' "${tag_ref}" | jq -r '.object.sha // empty')
if [ "${tag_object_type}" != "tag" ] || [ -z "${tag_object_sha}" ]; then
  echo "release tag ${RELEASE_VERSION} must be an annotated tag; lightweight tags are not accepted." >&2
  exit 1
fi

tag=$(gh api "repos/${GITHUB_REPOSITORY}/git/tags/${tag_object_sha}")
tag_target=$(printf '%s\n' "${tag}" | jq -r '.object.sha // empty')
tag_verified=$(printf '%s\n' "${tag}" | jq -r '.verification.verified // false')
if [ "${tag_target}" != "${GITHUB_SHA}" ]; then
  echo "release tag ${RELEASE_VERSION} does not resolve to release commit ${GITHUB_SHA}." >&2
  exit 1
fi

# v0.1.0 predates the tag-signature gate. Its commit is GitHub-verified and its
# immutable annotated tag object is deliberately named here so the exception
# cannot silently apply to a replacement or to any later release.
if [ "${RELEASE_VERSION}" = "v0.1.0" ] \
  && [ "${tag_object_sha}" = "442142d074f4de0584f58175642668a6f1ce3edf" ] \
  && [ "${tag_target}" = "78eb5a21b7c980107becedca6a9f7f6fc5528d06" ] \
  && [ "${tag_verified}" != "true" ]; then
  echo "release commit ${GITHUB_SHA} has a verified signature; v0.1.0's historical unsigned annotated tag is grandfathered."
  exit 0
fi

if [ "${tag_verified}" != "true" ]; then
  echo "release tag ${RELEASE_VERSION} lacks a GitHub-verified signature." >&2
  echo "Create it with 'git tag -s ${RELEASE_VERSION}' after registering the signing key; unsigned tags cannot publish a release." >&2
  exit 1
fi

echo "release commit ${GITHUB_SHA} and annotated tag ${RELEASE_VERSION} have verified signatures."
