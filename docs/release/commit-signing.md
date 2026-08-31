# Release commit signing

Every release lane calls `scripts/verify-release-commit-signature.sh`, which asks
the GitHub API for both the release commit and its annotated tag. It fails unless
both GitHub verification records are `true`. It is a hard gate with no bypass for
new releases.

## What the gate actually checks

It does **not** check for a specific key, a specific signer, or a local
`allowed_signers` file. It checks GitHub's own verification records for the
commit and annotated tag. GitHub sets `verified: true` in exactly two situations:

1. The commit carries an SSH or GPG signature made with a key the author has
   registered on their GitHub account as a **signing key**.
2. The commit was created by GitHub itself on behalf of an authenticated user —
   the web editor, a merge performed through the UI or API, or any write through
   the Contents API. GitHub signs those with its own key.

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

## The fix: register an SSH signing key

This is the steady state the repository's
[signed-history policy](signed-history-policy.md) assumes, and it is free.

The `gh` token in use lacks the `admin:ssh_signing_key` scope, so **this cannot
be done non-interactively** — `gh ssh-key add --type signing` will fail with a
scope error until the token is refreshed. Run these yourself, in order:

```sh
# 1. Grant the scope. This opens a browser and asks for confirmation.
gh auth refresh -h github.com -s admin:ssh_signing_key

# 2. Create a signing key. Use a passphrase; add it to your agent afterwards.
ssh-keygen -t ed25519 -C "thekozugroup@gmail.com" -f ~/.ssh/covalent_signing

# 3. Register it with GitHub as a SIGNING key, not an authentication key.
gh ssh-key add ~/.ssh/covalent_signing.pub --type signing --title "Covalent release signing"

# 4. Tell git to use it for every commit and tag.
git config --global gpg.format ssh
git config --global user.signingkey ~/.ssh/covalent_signing.pub
git config --global commit.gpgsign true
git config --global tag.gpgsign true
```

Verify before relying on it:

```sh
git commit --allow-empty -m "chore: verify commit signing"
git log -1 --pretty='%h %G? %s'          # expect G, not N
git push origin main
gh api repos/thekozugroup/Covalent/commits/main --jq '.commit.verification.verified'
```

That last command must print `true`. `%G?` printing `G` locally is necessary but
not sufficient — GitHub only reports `verified: true` once the *public* key is
registered on the account, so always confirm through the API.

Once signing is on, tag releases with `git tag -s`. The release workflow rejects
lightweight, unsigned, and mismatched annotated tags before it builds an artifact.

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
