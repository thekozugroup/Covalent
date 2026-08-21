# Release publishing runbook

Covalent has four publishing lanes. All of them are gated on the same source
conditions and all of them upload to the same GitHub Release for the tag.

| Lane | Workflow | Trigger | Credentials |
| --- | --- | --- | --- |
| Container (Unraid, Docker) | `container-supply-chain.yml` | `push` tag `v*` | none beyond `GITHUB_TOKEN` |
| macOS, unsigned | `apple-unsigned-release.yml` | `push` tag `v*` | none |
| macOS, notarized | `apple-release.yml` | `workflow_dispatch` | Apple Developer Program |
| Android, signed | `android-release.yml` | `workflow_dispatch` | self-provided keystore |

Every lane calls `scripts/publish-release-assets.sh`, which creates the release
for an already-existing tag (`--verify-tag`) if it is not there yet and then
uploads with `--clobber`. Lanes are therefore order-independent and re-runnable,
and the release page is assembled incrementally as each platform passes.

Release notes come from `docs/release/notes/<tag>.md` when that file exists.

## The version of record

`Cargo.toml` `[workspace.package] version` is the single source of truth.
`scripts/release-version.sh` derives and enforces every other surface:

- `apps/android/app/build.gradle.kts` — `versionName` and `versionCode`
- `apps/apple/project.yml` — `MARKETING_VERSION` and `CURRENT_PROJECT_VERSION`
  (the Apple `Info.plist` files are XcodeGen output and are gitignored, so
  `project.yml` is where the Apple version actually lives)
- `packaging/unraid/covalent.xml` — the `<Repository>` image tag

`versionCode` and `CURRENT_PROJECT_VERSION` are both the monotonic integer
`major * 1000000 + minor * 1000 + patch`, so they increase automatically and can
never regress across releases. `0.1.0` is build `1000`.

```sh
scripts/release-version.sh check        # fails on any drift
scripts/release-version.sh set 0.2.0    # rewrite every surface at once
```

`version-sync.yml` runs the check on every push and pull request, so drift fails
CI immediately rather than at tag time. Each release workflow runs it again
before building anything.

## Cutting a release

1. `scripts/release-version.sh set X.Y.Z` and `cargo update --workspace`.
2. Commit and push. Wait for all nine Tier 1 checks to go green on that commit.
3. Confirm the commit has a verified signature — see
   [commit-signing.md](commit-signing.md). This is a hard gate.
4. Write `docs/release/notes/vX.Y.Z.md`.
5. `git tag -s vX.Y.Z && git push origin vX.Y.Z`.
   That fires the container lane and the unsigned macOS lane.
6. Run `android-release.yml` with the same version. Set `first_release: true`
   only for the very first release, when no prior signed APK exists to upgrade
   from.
7. Run `apple-release.yml` once Apple credentials exist.
8. `gh release view vX.Y.Z` and confirm the assets are attached.

## One-time setup the maintainer must perform

These need either a browser or OAuth scopes that the current `gh` token does not
carry. Nothing in CI can do them.

### Android release keystore

The keystore is self-provided and free. **Back it up offline before you use it —
if you lose it you can never ship an update that installs over v0.1.0.**

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

### Make the GHCR package public

The Unraid template's `<Repository>` resolves only for a public package. The
first tagged container run creates the package as **private**. After that run:

<https://github.com/users/thekozugroup/packages/container/covalent/settings>
→ *Danger Zone* → *Change visibility* → **Public**.

There is no API for this that the current token scopes reach; it is a one-time
browser action per package.

Confirm with an unauthenticated pull:

```sh
docker logout ghcr.io
docker pull ghcr.io/thekozugroup/covalent:v0.1.0
```

### Apple Developer Program (blocks the notarized lane only)

`apple-release.yml` hard-requires six secrets and none of them can be
self-provided:

| Secret | Source |
| --- | --- |
| `APPLE_TEAM_ID` | Apple Developer account |
| `DEVELOPER_ID_P12_BASE64` | Developer ID Application certificate, exported `.p12` |
| `DEVELOPER_ID_P12_PASSWORD` | chosen at export |
| `APPLE_NOTARY_KEY_BASE64` | App Store Connect API key `.p8` |
| `APPLE_NOTARY_KEY_ID` | App Store Connect |
| `APPLE_NOTARY_ISSUER_ID` | App Store Connect |

Until then, `apple-unsigned-release.yml` ships an ad-hoc signed Apple Silicon
build that is honestly labelled as unsigned in the release notes. It asserts
`Signature=adhoc` and asserts that no notarization ticket is present, so it can
never be mistaken for the notarized lane.

### Unraid Community Applications

After the GHCR package is public, submit `packaging/unraid/covalent.xml` to the
Community Applications template feed. That is a human review cycle, not an
automatable step.
