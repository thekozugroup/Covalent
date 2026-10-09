# Release publishing runbook

Covalent has three publishing lanes in the current personal-use release scope.
All of them are gated on the same source conditions and upload to the same
GitHub Release for the tag.

| Lane | Workflow | Trigger | Credentials |
| --- | --- | --- | --- |
| Container (Unraid, Docker) | `container-supply-chain.yml` | `push` tag `v*` | none beyond `GITHUB_TOKEN` |
| macOS, unsigned | `apple-unsigned-release.yml` | `push` tag `v*` | none |
| Trusted CLI | `cli-release.yml` | `push` tag `v*` | GitHub OIDC keyless signing |

The installable signed Android APK is built through the personal-use setup
path, tested, and attached to the same draft release with its checksum and build
evidence. v0.2.12 uses the minified, non-debuggable personal release certificate; include its upgrade note for older debug-signed installations. Android production signing is deferred. The Apple
Developer ID/notarization workflow is outside this release scope and must not
be run.

Every lane calls `scripts/publish-release-assets.sh`, which creates the release
for an already-existing tag (`--verify-tag`) if it is not there yet. The helper
discovers drafts through the authenticated releases list, resolves the numeric
release ID, and replaces assets through numeric release/asset endpoints. Lanes
are therefore order-independent and re-runnable even while the by-tag endpoint
still returns 404, and the release page is assembled incrementally as each
platform passes.

Container reruns are fail-closed. The workflow serializes all release refs,
signs and verifies a non-consumer candidate digest before any public tag moves,
and treats `vX.Y.Z` as immutable. Its OCI index carries the exact stable version;
`latest` moves only when that version is newer, stays put for the same version
and digest, and refuses an older version, an equal-version digest mismatch, or
unknown version provenance. A one-time exact digest mapping recognizes the
published v0.1.0 index, which predates the version annotation.

The release is created as a **draft**, and no lane ever publishes it. Assembly is
incremental, but visibility is not: no lane knows whether it is the last to
finish, so a lane that published on its own way out would expose a
half-assembled page. The authorized publisher reviews the complete draft before
publishing it — see steps 9 and 10 below.

Release notes come from `docs/release/notes/<tag>.md` when that file exists.

## v0.2.1 packaging repair

The runtime source tag `v0.2.1` remains immutable. CLI signing/publication repair
uses the separate annotated signed `release-tools-v0.2.1` tag. Container
signing/publication repair uses the separate annotated signed
`release-tools-v0.2.1-container` tag. These are exceptions for v0.2.1 only;
they repair packaging and publication without moving the runtime tag.

The repair must reuse the original CLI archive/SBOM bytes and scanned container
architecture archives from their source-bound workflow runs. Record those run
IDs, artifact hashes and source revision in the final provenance. Do not rebuild
runtime code or relabel different bytes as the original source. The repair
workflows must verify the handoffs before signing and publishing.

Require these exact GitHub OIDC certificate identities for repaired v0.2.1
assets, with issuer `https://token.actions.githubusercontent.com`:

- CLI: `https://github.com/thekozugroup/Covalent/.github/workflows/cli-release.yml@refs/tags/release-tools-v0.2.1`
- Container: `https://github.com/thekozugroup/Covalent/.github/workflows/container-supply-chain.yml@refs/tags/release-tools-v0.2.1-container`

The runtime tag controls the application version and source; the repair tag
identifies the reviewed packaging code. Both must retain verified signatures.
Do not broaden the trust rule to a branch, arbitrary repair tags or an identity
regular expression. Other releases continue to use their exact version-tag
workflow identity. The CLI install guide and container verification instructions
must use the same explicit v0.2.1 exception, including SBOM attestation checks.
The verified v0.2.1 container digest is
`sha256:393f8a0dafa7f17d8ad964d501f3d33668d547889dc493080e042489ee3e1677`;
use the matching `covalent-container-digest.txt` when completing step 11.

## v0.2.12 registry build repair

The signed runtime tag `v0.2.12` remains at
`7bb83c27ed56d2b78517845db7bdf1757b80fbe1`. All eleven source gates passed,
including 202 Android unit tests and the 87-test API 37 suite. Its first
container publication attempts failed on Docker Hub HTTP 429 responses before
any public image promotion.

The separate signed `release-tools-v0.2.12-container-2` tag repairs only the build
and publication process. Dispatch `container-supply-chain.yml` from that tag
with `version=v0.2.12` and no `artifact_run_id`. The repair gate accepts only
this runtime SHA, version and annotated signed repair tag; it also verifies the
repair commit's signature. Other releases continue to use their normal signed
version tag. The historical v0.2.1 archive-recovery exception stays unchanged.

The builder checks out the original runtime into `release-source`, fingerprints
it before and after the build, and labels both images with that runtime SHA.
BuildKit uses [Docker's documented mirror configuration](https://docs.docker.com/build/ci/github-actions/configure-builder/#registry-mirror).
The original Go, Rust and Alpine image digests remain pinned. Google's mirror
returned the exact index hashes for all three. The temporary registry has a
fallback to the same pinned digest on the mirror. Both architecture scans,
checksummed handoff, signatures, exact SBOM attestations and immutable promotion
rules remain required.

The container's exact GitHub OIDC identity is
`https://github.com/thekozugroup/Covalent/.github/workflows/container-supply-chain.yml@refs/tags/release-tools-v0.2.12-container-2`,
with issuer `https://token.actions.githubusercontent.com`. CLI artifacts retain
their normal `refs/tags/v0.2.12` identity. Do not replace either with a branch or
an identity regular expression. The completed container digest receipt records
its runtime commit and the repair signing identity separately.

The first mirror repair still timed out while the Docker daemon fetched BuildKit
from Docker Hub, before the configured mirror could run. The second immutable
repair tag pins the BuildKit multi-platform image from the mirror at
`sha256:cec9f139f45e93c5c69c60f8b07cfad9f43f4ef6b6a6cd917527fea5ff2e3dea`.
The first repair tag remains unchanged. The signed runtime tag and its Docker
source inputs remain unchanged.

## The version of record

`Cargo.toml` `[workspace.package] version` is the single source of truth.
`scripts/release-version.sh` derives and enforces every other surface:

- `apps/android/app/build.gradle.kts` — `versionName` and `versionCode`
- `apps/apple/Project.yml` — `MARKETING_VERSION` and `CURRENT_PROJECT_VERSION`
  (the Apple `Info.plist` files are XcodeGen output and are gitignored, so
  `Project.yml` is where the Apple version actually lives)

The Unraid template is intentionally not version-derived. It pins an immutable
published image digest, which is only known after the container scan/sign/attest
lane succeeds. `scripts/release-version.sh set` must never replace that digest
with a mutable tag.

`versionCode` and `CURRENT_PROJECT_VERSION` are both the monotonic integer
`major * 1000000 + minor * 1000 + patch`, so they increase automatically and can
never regress across releases. `0.1.0` is build `1000`; `0.2.0` is build
`2000`; `0.2.1` is build `2001`.

```sh
scripts/release-version.sh check        # fails on any drift
scripts/release-version.sh set 0.2.1    # rewrite every surface at once
```

`version-sync.yml` runs the check on every push and pull request, so drift fails
CI immediately rather than at tag time. Each release workflow runs it again
before building anything.

## Cutting a release

1. `scripts/release-version.sh set X.Y.Z` and `cargo update --workspace`. Write
   `docs/release/notes/vX.Y.Z.md` before committing.
2. Commit and push. Wait for every required Tier 1 check to go green on that
   exact commit, including the release notes.
3. Confirm the commit has a verified signature — see
   [commit-signing.md](commit-signing.md). This is a hard gate.
4. Confirm the release notes and version files belong to that verified commit;
   any further source change needs a new commit and its required checks.
5. Create an **annotated signed** tag: `git tag -s vX.Y.Z && git push origin vX.Y.Z`.
   That fires the container lane and the unsigned macOS lane.
6. Build an installable, signed personal-use Android APK from the tagged source using
   [the Android setup guide](../platform/android.md). Verify its package identity
   and provenance, then attach it and its `.sha256` file to the same draft using
   `scripts/publish-release-assets.sh vX.Y.Z FILE...`. Include the Android SBOM,
   license inventory, notices, notice manifest, and build receipt recording the
   source commit, source fingerprint, APK and native hashes, and signing
   certificate SHA-256. Include the existing Android emulator acceptance receipt
   and identify its tested source and APK separately from the final package.
   Record source equivalence for the behavior covered by that receipt. Under the
   owner's 2026-09-20 threshold, working apps and at least one tested sync
   combination suffice; a repeated exact-APK UI journey is required only when
   a material change invalidates the existing behavior evidence. Do not label
   the final APK as device-tested if only a prior candidate was exercised, or
   claim physical Android coverage. Use the existing authenticated publishing
   environment required by the helper; do not put tokens in files or logs.
   The debug-signed personal builder remains available. A minified, non-debuggable
   personal release APK may use a persistent local release certificate after
   the Android gate and signed assembly pass. Record compatibility with earlier
   published APKs and include an explicit migration note for any mismatch.
   Never publish an unsigned APK. Store publication remains deferred.
7. The CLI lane runs on the tag and publishes source-free Linux amd64, Linux
   arm64, and Apple Silicon macOS arm64 archives only after all three pass
   binary architecture/size, license inventory, SPDX SBOM, Sigstore signature,
   and exact attestation gates. Build jobs have no OIDC access; separate clean
   jobs verify checksummed handoffs before signing, and publication requires
   each signed archive digest and embedded SPDX predicate to equal the released
   archive and SBOM. See [CLI installation](cli-install.md).
8. Do not run the Developer ID workflow. The ad-hoc Apple Silicon artifact from
   `apple-unsigned-release.yml` is the defined personal-use macOS package.
9. Find the draft through the authenticated release list and confirm every
   expected asset is attached, with the sizes and checksums you expect. Do not
   use the by-tag endpoint for this pre-publish check:

   ```sh
   version=vX.Y.Z
   release_id=$(gh api --paginate --slurp \
     'repos/OWNER/REPO/releases?per_page=100' \
     | jq -r --arg version "$version" '.[][] | select(.tag_name == $version) | .id')
   gh api "repos/OWNER/REPO/releases/${release_id}" \
     --jq '.assets[] | [.name, .size] | @tsv'
   ```

   **Expect the tag URL to 404 at this point.** A draft is not bound to its tag,
   so until step 10 runs:

   ```console
   $ gh api repos/OWNER/REPO/releases/tags/vX.Y.Z
   gh: Not Found (HTTP 404)
   ```

   and `https://github.com/OWNER/REPO/releases/tag/vX.Y.Z` 404s in a browser,
   even though the draft plainly exists. That is normal and does **not** mean a
   lane failed to publish. The draft lives at a `releases/tag/untagged-<hash>`
   URL; find it with `gh release list` or `gh api repos/OWNER/REPO/releases`,
   both of which do show drafts. It is also invisible to anyone without push
   access. Both the tag URL and the by-tag API lookup start working the moment
   you run step 10.
10. Publish it, and only then:

   ```sh
   gh release edit vX.Y.Z --draft=false
   ```

   Do not skip this. `gh release view` with no tag argument means "latest
   published release" and does not see drafts, so a release left in draft is
   invisible to the `android-release.yml` upgrade gate, which will then
   correctly refuse to run without `first_release`.

11. After the container lane passes, copy the immutable digest from
    `covalent-container-digest.txt` into `packaging/unraid/covalent.xml`, run
    `./scripts/validate-unraid-template.sh`, and commit that template update.
    The template tracks the scanned release artifact, not a mutable version tag.

## One-time setup the maintainer must perform

These need either a browser or OAuth scopes that the current `gh` token does not
carry. Nothing in CI can do them.

### Android production keystore (deferred)

This is not required for the current personal-use release. If production
Android signing is added later, create and escrow a dedicated release key
before publishing the first production-signed APK. A debug-signed personal APK
cannot update to a differently signed production APK in place.

```sh
keytool -genkeypair -v \
  -keystore covalent-release.keystore \
  -alias covalent \
  -keyalg RSA -keysize 4096 -validity 10000 \
  -dname "CN=Covalent, O=The Kozu Group, C=US"

base64 -i covalent-release.keystore | tr -d '\n' > covalent-release.keystore.b64

gh secret set COVALENT_ANDROID_KEYSTORE_BASE64 --repo thekozugroup/Covalent < covalent-release.keystore.b64
gh secret set COVALENT_ANDROID_STORE_PASSWORD  --repo thekozugroup/Covalent
gh secret set COVALENT_ANDROID_KEY_ALIAS       --repo thekozugroup/Covalent   # covalent
gh secret set COVALENT_ANDROID_KEY_PASSWORD    --repo thekozugroup/Covalent

rm covalent-release.keystore.b64
```

### GHCR package visibility

The Unraid template's immutable `<Repository>` resolves only for a public
package. v0.1.0 is publicly pullable. Keep the package public for future
releases; if it is ever recreated as private, change it in:

<https://github.com/users/thekozugroup/packages/container/covalent/settings>
→ *Danger Zone* → *Change visibility* → **Public**.

There is no API for this that the current token scopes reach; it is a one-time
browser action per package.

Confirm with an unauthenticated pull by digest:

```sh
docker logout ghcr.io
# Use the digest file from the completed, verified release lane.
digest=$(sed -n 's/^digest: //p' covalent-container-digest.txt)
printf '%s\n' "$digest" | grep -Eq '^ghcr\.io/thekozugroup/covalent@sha256:[0-9a-f]{64}$' || exit 1
docker pull "$digest"
```

### macOS personal-use package

Apple Developer ID and notarization are excluded from this release. Do not
create, request, or configure Apple signing credentials. The
`apple-unsigned-release.yml` lane ships the ad-hoc-signed Apple Silicon build,
labels it honestly, asserts `Signature=adhoc`, and asserts that no notarization
ticket is present. Users follow the explicit Gatekeeper-safe personal-use steps
in [the macOS setup guide](../platform/macos.md).

### Unraid Community Applications

After the GHCR package is public, submit `packaging/unraid/covalent.xml` to the
Community Applications template feed. That is a human review cycle, not an
automatable step.
