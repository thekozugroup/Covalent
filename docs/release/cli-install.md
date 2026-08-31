# Install the Covalent CLI

No verified CLI archive is published with the historical v0.1.0 release. This
guide applies only after the replacement signed release attaches its CLI
archives. Until then, do not treat this page as an install path or use it to
claim a v0.1.0 server.

Once published, the archive is the supported way to run `covalent claim` on a
trusted Mac or Linux computer. It needs no source build and no installer script.
Use the archive that exactly matches the computer running the command:

| Computer | Archive suffix |
| --- | --- |
| Intel/AMD Linux | `linux-amd64.tar.gz` |
| ARM64 Linux | `linux-arm64.tar.gz` |
| Apple Silicon macOS | `macos-arm64.tar.gz` |

Windows and Intel macOS are unsupported. The macOS CLI is arm64-only.

## Download and verify

For a published `vX.Y.Z` release, download these three files from the GitHub
release page into an empty directory:

- `Covalent-vX.Y.Z-<platform>.tar.gz`
- `Covalent-vX.Y.Z-<platform>-SHA256SUMS.txt`
- `Covalent-vX.Y.Z-<platform>.tar.gz.sigstore.json`

The release also includes an attested SBOM, inventory, and attestation bundle for
audit. Do not use `curl | sh`, skip checksum validation, or bypass the Sigstore
identity check.

On macOS, `shasum` is already installed. On Linux use `sha256sum` when
available; `shasum -a 256` is an equivalent portable fallback.

```sh
version=vX.Y.Z
platform=macos-arm64 # or linux-amd64, linux-arm64
archive="Covalent-${version}-${platform}.tar.gz"
manifest="Covalent-${version}-${platform}-SHA256SUMS.txt"

# The manifest covers the archive, both Sigstore bundles, SBOM, and inventory.
# Verify only the exact archive line when those audit assets were not downloaded.
awk -v archive="${archive}" '$2 == archive || $2 == "./" archive { print }' \
  "${manifest}" > "${archive}.sha256"
test "$(wc -l < "${archive}.sha256" | tr -d ' ')" = 1
read -r expected_digest listed_archive < "${archive}.sha256"
case "${listed_archive}" in
  "${archive}"|"./${archive}") ;;
  *) exit 1 ;;
esac
test "${#expected_digest}" = 64
case "${expected_digest}" in *[!0-9a-f]*) exit 1 ;; esac
printf '%s  %s\n' "${expected_digest}" "${archive}" | shasum -a 256 -c -
cosign verify-blob \
  --bundle "${archive}.sigstore.json" \
  --certificate-identity "https://github.com/thekozugroup/Covalent/.github/workflows/cli-release.yml@refs/tags/${version}" \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  "${archive}"
```

This identity check ensures the archive was keylessly signed by this repository
in GitHub Actions. A checksum alone detects accidental corruption; it does not
establish who produced the archive.

To audit the SBOM attestation too, also download
`${archive}.attestation.sigstore.json` and
`Covalent-${version}-${platform}-sbom.spdx.json`. From an exact checkout of the
same signed release tag, run the repository verifier:

```sh
sbom="Covalent-${version}-${platform}-sbom.spdx.json"
scripts/verify-cli-release-attestation.sh \
  "${archive}.attestation.sigstore.json" "${archive}" "${sbom}" \
  "https://github.com/thekozugroup/Covalent/.github/workflows/cli-release.yml@refs/tags/${version}" \
  https://token.actions.githubusercontent.com
```

This first performs Sigstore verification, then independently requires the
signed in-toto subject to equal the archive SHA-256 and the signed SPDX
predicate to canonically equal the downloaded SBOM. Verifying only the bundle
signature does not establish that the separately published SBOM has the same
contents.

## Extract and run

```sh
tar -xzf "${archive}"
mkdir -p "$HOME/.local/bin"
install -m 0755 "Covalent-${version}-${platform}/covalent" "$HOME/.local/bin/covalent"
"$HOME/.local/bin/covalent" --help
```

Add `$HOME/.local/bin` to `PATH` through the normal shell settings for the
computer, then use `covalent claim` as documented in the
[Atlas/Tailscale runbook](../platform/atlas-tailscale.md). Keep the claimed CA
and token in the owner-only output directory created by that command.

## What a complete release contains

Each platform archive has its own SHA256SUMS file, Sigstore signature bundle,
SPDX SBOM, dependency/license inventory, and Sigstore SBOM attestation. The
release workflow builds all three archives without OIDC access, passes them
through checksummed handoffs to clean signing jobs, then verifies architecture,
the 8 MiB CLI size budget, checksums, signatures, exact archive subjects, exact
SBOM predicates, declared licenses, and the signed tag commit before publishing
the assets to the draft release.
