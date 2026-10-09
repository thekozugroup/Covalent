# Release commit signing

Every release lane calls `scripts/verify-release-commit-signature.sh`, which asks
the GitHub API for both the release commit and its annotated tag. It fails unless
both GitHub verification records are `true`. It is a hard gate with no bypass for
new releases.

## What the gate actually checks

It does **not** check for a specific key, a specific signer, or a local
`allowed_signers` file. It checks GitHub's own verification records for the
commit and annotated tag. The relevant supported signing paths are:

1. The commit carries an SSH or GPG signature made with a key the author has
   registered on their GitHub account as a **signing key**.
2. The commit was created through a GitHub workflow that signs on behalf of an
   authenticated user, such as the GraphQL `createCommitOnBranch` mutation used
   for this repository's initial release. Confirm the resulting verification
   record; a server-side write alone does not guarantee a signed commit. The
   REST Git Data and Contents APIs must not be assumed to sign it.

Case 2 is observable in this repository today. Every Dependabot commit on the
`dependabot/*` branches reports:

```console
$ gh api repos/thekozugroup/Covalent/commits/1234286b --jq '.commit.verification'
{"verified": true, "reason": "valid", ...}
```

while every commit on `main` reports `verified: false` / `%G? = N`, because they
were all pushed from a local git client with no registered signing key.

So the gate is satisfiable without any paid credential. It is **not** satisfiable
without a one-time human action: either registering a signing key, or making the
release commit through GitHub rather than through `git push`.

## What v0.1.0 actually used, and why it is the weaker of the two

`v0.1.0` was cut on a commit created through case 2 — GitHub's GraphQL
`createCommitOnBranch` mutation, which writes the commit server-side and signs it
with GitHub's web-flow key:

```sh
gh api graphql --input commit.json   # mutation createCommitOnBranch(...)
gh api repos/thekozugroup/Covalent/commits/<sha> --jq '.commit.verification'
# {"verified": true, "reason": "valid", ...}   committer: GitHub <noreply@github.com>
```

Note that the REST Contents API (`PUT /repos/{owner}/{repo}/contents/{path}`)
does **not** produce a signed commit for this account — it was tried first and
returned `verified: false, reason: "unsigned"`. Only the GraphQL mutation signs.

This satisfies the commit gate legitimately and without weakening it, but be clear about
what it attests. A GitHub web-flow signature proves the commit was created
through an authenticated API call on the maintainer's GitHub account. It does not
prove that a key the maintainer holds signed the commit content. An attacker with
a stolen `gh` token can produce a `verified: true` commit; an attacker without the
maintainer's private signing key cannot produce case 1.

Case 1 is therefore the intended steady state and this route is a bootstrap for
the first release only. `v0.1.0` has an unsigned annotated tag
(`442142d074f4de0584f58175642668a6f1ce3edf`) and is the sole grandfathered
exception in the verifier. The exception is tied to that exact tag object and
commit; it cannot approve a replacement tag or any later release. Register the
SSH signing key below and all later releases carry stronger commit and tag
attestation.

## Repository setup

This is the steady state the repository's
[signed-history policy](signed-history-policy.md) assumes, and it is free.

Register a dedicated public key as a GitHub **signing key**, not an
authentication key, then enable SSH signing in this repository:

```sh
gh auth refresh -h github.com -s admin:ssh_signing_key
ssh-keygen -t ed25519 -C "thekozugroup@gmail.com" -f ~/.ssh/covalent_signing
gh ssh-key add ~/.ssh/covalent_signing.pub --type signing --title "Covalent release signing"
git config --local gpg.format ssh
git config --local user.signingkey ~/.ssh/covalent_signing
git config --local commit.gpgsign true
git config --local tag.gpgsign true
```

The current repository completed that setup on 2026-09-20. The signing-only
key is registered to `thekozugroup`; repository-local commit and tag signing are
enabled. The receipt is
`artifacts/validation-2026-09-20/release-signing-setup.json`. It records the
public fingerprint and configuration, never private-key contents. Keep the
private key persistent and protected; it is release identity, not temporary
test data.

Confirm configuration without printing private material:

```sh
git config --local --get gpg.format           # ssh
git config --local --get user.signingkey      # path only
git config --local --get commit.gpgsign       # true
git config --local --get tag.gpgsign          # true
gh api user/ssh_signing_keys \
  --jq '.[] | select(.title == "Covalent release signing") | {id,title}'
```

Sign the real release commit normally. Do not create an empty verification
commit or push directly to `main` only to test the key:

```sh
git commit -S -m "release: prepare v0.2.1"
git log -1 --show-signature --pretty=fuller
git push origin codex/production-readiness
release_commit=$(git rev-parse HEAD)
gh api "repos/thekozugroup/Covalent/commits/${release_commit}" \
  --jq '.commit.verification | {verified,reason}'
```

The API must report `verified: true` and `reason: valid`. A local signature is
necessary but not sufficient because release workflows use GitHub's record.

After the exact intended release commit is pushed, GitHub-verified, and passes
its required checks, create and verify the annotated signed tag. The release
workflows do not require that commit to be on `main`; no PR merge is needed:

```sh
release_commit=$(git rev-parse HEAD)
gh api "repos/thekozugroup/Covalent/commits/${release_commit}" \
  --jq '.commit.verification | {verified,reason}'
git tag -s v0.2.1 -m "Covalent v0.2.1"
git verify-tag v0.2.1
test "$(git rev-list -n 1 v0.2.1)" = "${release_commit}"
git push origin v0.2.1
```

Pushing the tag starts `container-supply-chain.yml`,
`apple-unsigned-release.yml`, and `cli-release.yml`. The personal-release scope
does not run `apple-release.yml` or `android-release.yml`: Developer ID and
Android production signing remain deferred. Review all draft assets, checksums,
and workflow results before publishing through the commands in
`publishing.md`. The workflows reject lightweight, unsigned, mismatched, or
GitHub-unverified tags before building release artifacts.

## Why the gate was not weakened

Three options were considered and rejected:

- Deleting the call from the release workflows. That removes the only control
  tying a published binary to an identified author.
- Allowing `verified: false` when a `reason` is "no signature". Same effect,
  more indirection.
- Adding an `allowed_signers` file and verifying locally with `git verify-commit`.
  This does not help: the script reads GitHub's record, and a local
  `allowed_signers` file has no bearing on it. Rewriting the script to verify
  locally instead would *lower* the bar, because the release runner would then be
  trusting a file that lives in the same repository it is releasing.

The gate stays fail-closed. The release workflows now fail with the script's own
message for an unsigned commit, lightweight tag, unsigned tag, or tag/commit
mismatch, which points at this document.
