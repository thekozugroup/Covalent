# Release commit signing

Every credentialed release lane calls `scripts/verify-release-commit-signature.sh`,
which asks the GitHub API for the release commit and fails unless
`.commit.verification.verified` is `true`. It is a hard gate with no bypass, and
it must stay that way.

## What the gate actually checks

It does **not** check for a specific key, a specific signer, or a local
`allowed_signers` file. It checks GitHub's own verification record for the
commit. GitHub sets `verified: true` in exactly two situations:

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

Once signing is on, tag releases with `git tag -s`.

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

The gate stays exactly as written. The release workflows now fail with the
script's own message — `release commit <sha> lacks a verified signature (<reason>)`
— which points at this document.
