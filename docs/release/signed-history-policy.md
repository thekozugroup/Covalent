# Signed-history release policy

Covalent keeps direct `main` maintenance available to the repository owner. That bypass must not permit an unsigned production release.

## Required ruleset shape

Configure two GitHub rulesets for `main`:

1. The delivery ruleset requires linear history, forbids deletion and non-fast-forward updates, and requires the Tier 1 release contexts. It may retain the repository-owner direct-push bypass.
2. The signature ruleset requires signed commits and has no bypass actor. This preserves direct-main delivery while applying the same signature standard to the owner and every other writer.

Do not merge these rulesets while the owner bypass applies to required signatures: GitHub evaluates the bypass for every rule in a ruleset.

## Release fail-closed gate

Each credentialed Android, macOS, and container release workflow calls `scripts/verify-release-commit-signature.sh`. The script uses the GitHub commit-verification record and rejects a release commit unless `verification.verified` is true. It also requires the exact-commit `CodeQL policy` check, which rejects every open CodeQL alert.

Before enabling the no-bypass signature ruleset, register the maintainer's signing key with GitHub, enable signed commits locally, and make one signed non-release commit to verify the repository's identity and release workflow permissions. This repository change does not alter remote rulesets.
