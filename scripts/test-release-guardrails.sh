#!/bin/sh
# Static contract for release safeguards. It deliberately does not build an app
# or contact GitHub, so it can run on every contributor machine.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"

./scripts/validate-unraid-template.sh
./scripts/test-atlas-preflight.sh
./scripts/release-version.sh check

# Keep the public protocol contract and the v0.2 upgrade boundary derived from
# the transport implementation instead of allowing another silent doc drift.
transport_version=$(sed -n \
  's/^pub const QUIC_TRANSPORT_VERSION: u16 = \([0-9][0-9]*\);$/\1/p' \
  crates/covalent-node/src/transport.rs)
case "$transport_version" in
  ''|*[!0-9]*)
    echo "could not derive the QUIC transport version" >&2
    exit 1
    ;;
esac
grep -Fq "const ALPN: &[u8] = b\"covalent-quic/${transport_version}\";" crates/covalent-node/src/transport.rs
grep -Fq "const TRANSPORT_SIGNATURE_DOMAIN: &[u8] = b\"covalent/authenticated-quic/v${transport_version}\";" crates/covalent-node/src/transport.rs
grep -Fq "transport v${transport_version}" README.md
grep -Fq "QUIC transport v${transport_version}" docs/protocol/protocol.md
grep -Fq "\`covalent-quic/${transport_version}\`" docs/protocol/protocol.md
grep -Fq "\`covalent/authenticated-quic/v${transport_version}\`" docs/protocol/protocol.md
grep -Fq 'v0.1.0 transport-v2 peers' docs/protocol/protocol.md
grep -Fq 'v0.1.0 peers' docs/release/notes/v0.2.0.md
grep -Fq 'speak QUIC transport v2 while v0.2.0 peers speak transport v3' docs/release/notes/v0.2.0.md
if grep -qi 'unraid' scripts/release-version.sh; then
  echo "release-version.sh must not replace the immutable Unraid image digest" >&2
  exit 1
fi

if rg -q '/mnt/disks/covalent-secrets|Unassigned Devices-style secret path' \
  packaging/unraid/covalent.xml docs/platform/unraid.md; then
  echo "Unraid KEK documentation must not depend on Unassigned Devices" >&2
  exit 1
fi
rg -q '/mnt/user/system/covalent-secrets' packaging/unraid/covalent.xml
rg -q 'never map /mnt/user/system' packaging/unraid/covalent.xml
rg -q 'Never select `/mnt/user/system`' docs/platform/unraid.md
grep -Fq 'KEK_DIR=/mnt/user/system/covalent-secrets' docs/platform/atlas-tailscale.md
grep -Fq 'docker run --rm --user 99:100' docs/platform/atlas-tailscale.md
if grep -Fq '/srv/covalent/secrets' docs/platform/atlas-tailscale.md; then
  echo "Atlas must use the exact Unraid KEK path" >&2
  exit 1
fi
test -x scripts/atlas-preflight.sh
test -x scripts/atlas-preflight-remote.sh
test -x scripts/test-atlas-preflight.sh
grep -Fq 'sh -s -- "$digest" "$atlas_host" /mnt/user "$@" < "$remote_helper"' scripts/atlas-preflight.sh
grep -Fq 'atlas.example-tailnet.ts.net' docs/platform/atlas-tailscale.md
private_host_fragment='taila''7985'
if rg -q "$private_host_fragment" docs scripts crates/covalent-cli/src/main.rs; then
  echo "personal Atlas hostname must not appear in release source" >&2
  exit 1
fi
grep -Fq "tcp_listeners=\$(ss -H -lnt 'sport = :8443')" scripts/atlas-preflight-remote.sh
grep -Fq "udp_listeners=\$(ss -H -lnu 'sport = :8787')" scripts/atlas-preflight-remote.sh
grep -Fq 'source must be canonical and contain no symlink components' scripts/atlas-preflight-remote.sh
grep -Fq 'source root must be the Unraid /mnt/user boundary' scripts/atlas-preflight-remote.sh
grep -Fq 'require_mode /source ro' scripts/validate-unraid-template.sh
grep -Fq 'require_mode /boot-source ro' scripts/validate-unraid-template.sh
grep -Fq 'require_mode /run/secrets/covalent-kek ro' scripts/validate-unraid-template.sh

image_digest=$(sed -n 's|^[[:space:]]*<Repository>.*@\(sha256:[0-9a-f]*\)</Repository>[[:space:]]*$|\1|p' packaging/unraid/covalent.xml)
grep -Fq "$image_digest" docs/release/notes/v0.1.0.md
grep -Fq "$image_digest" docs/platform/atlas-tailscale.md
historical_container_identity='https://github.com/thekozugroup/Covalent/.github/workflows/container-supply-chain.yml@refs/tags/v0.1.0'
grep -Fq -- "--certificate-identity '$historical_container_identity'" docs/platform/atlas-tailscale.md
grep -Fq -- "--certificate-identity '$historical_container_identity'" docs/release/notes/v0.1.0.md
if rg -q -- '--certificate-identity-regexp.*thekozugroup/Covalent' \
  docs/platform/atlas-tailscale.md docs/release/notes/v0.1.0.md; then
  echo "container install documentation must pin the exact workflow and tag identity" >&2
  exit 1
fi

for workflow in \
  .github/workflows/container-supply-chain.yml \
  .github/workflows/apple-unsigned-release.yml \
  .github/workflows/cli-release.yml
do
  grep -q 'RELEASE_VERSION=.*verify-release-commit-signature.sh' "$workflow"
done

workflow_job() {
  awk -v header="  $2:" '
    $0 == header { emit = 1 }
    emit && $0 ~ /^  [A-Za-z0-9_-]+:$/ && $0 != header { exit }
    emit { print }
  ' "$1"
}

cli_workflow=.github/workflows/cli-release.yml
# A manual retry must execute on the tag itself, not merely name a tag that
# happens to point at the selected branch commit. Cosign records this ref.
grep -Fq 'GITHUB_REF}" != "refs/tags/${version}"' "$cli_workflow"
grep -Fq 'GITHUB_REF_TYPE}" != "tag"' "$cli_workflow"
grep -Fq 'certificate_identity="https://github.com/${GITHUB_REPOSITORY}/.github/workflows/cli-release.yml@refs/tags/${RELEASE_VERSION}"' "$cli_workflow"
test -x scripts/package-cli-release.sh
test -x scripts/generate-cli-release-inventory.sh
test -x scripts/test-cli-release-workflow.sh
./scripts/test-cli-release-workflow.sh

# Drafts are discovered from the authenticated list and uploads use the
# numeric release ID. The fixture makes tag-endpoint regressions executable.
test -x scripts/test-publish-release-assets.sh
./scripts/test-publish-release-assets.sh
grep -Fq 'release_identity=$(ensure_release)' scripts/publish-release-assets.sh
grep -Fq 'repos/${GITHUB_REPOSITORY}/releases/${release_id}/assets?per_page=100' scripts/publish-release-assets.sh
grep -Fq '"${release_upload_url}?name=${asset_name}"' scripts/publish-release-assets.sh
if grep -Eq '^[[:space:]]*gh release (view|upload)([[:space:]]|$)' scripts/publish-release-assets.sh; then
  echo "draft release publishing must not depend on a tag-addressed read or upload" >&2
  exit 1
fi

# Container scan evidence must be built locally, scanned before a GHCR login,
# and handed to the credentialed promotion job only with checksums and its
# locally reconstructed index digest.
container_workflow=.github/workflows/container-supply-chain.yml
grep -q 'outputs: type=docker,dest=${{ runner.temp }}/covalent-' "$container_workflow"
grep -q 'image: covalent-private:amd64' "$container_workflow"
grep -q 'image: covalent-private:arm64' "$container_workflow"
test "$(grep -Fc 'uses: anchore/sbom-action@' "$container_workflow")" -eq 2
grep -Fq 'output-file: covalent-container-linux-amd64.spdx.json' "$container_workflow"
grep -Fq 'output-file: covalent-container-linux-arm64.spdx.json' "$container_workflow"
if grep -Fq 'covalent-container.spdx.json' "$container_workflow"; then
  echo "container release evidence must not collapse both architectures into one host-selected SBOM" >&2
  exit 1
fi
scan_job=$(awk '
  $0 == "  scan-private:" { emit = 1 }
  emit && $0 ~ /^  [A-Za-z0-9_-]+:$/ && $0 != "  scan-private:" { exit }
  emit { print }
' "$container_workflow")
promote_job=$(awk '
  $0 == "  promote-sign:" { emit = 1 }
  emit && $0 ~ /^  [A-Za-z0-9_-]+:$/ && $0 != "  promote-sign:" { exit }
  emit { print }
' "$container_workflow")
if [ -z "$scan_job" ] || [ -z "$promote_job" ]; then
  echo "container workflow must keep separate scan-private and promote-sign jobs" >&2
  exit 1
fi
if ! printf '%s\n' "$scan_job" | grep -q 'contents: read' \
  || printf '%s\n' "$scan_job" | grep -Eq 'packages: write|id-token: write|docker/login-action'; then
  echo "private container scan job has an unnecessary publishing credential" >&2
  exit 1
fi
if ! printf '%s\n' "$promote_job" | grep -q 'packages: write' \
  || ! printf '%s\n' "$promote_job" | grep -q 'id-token: write' \
  || printf '%s\n' "$promote_job" | grep -Eq 'anchore/scan-action|anchore/sbom-action'; then
  echo "promotion job must have publish credentials but no scan action" >&2
  exit 1
fi
scan_line=$(grep -n 'Scan private linux/amd64 image' "$container_workflow" | cut -d: -f1)
promote_line=$(grep -n '^  promote-sign:' "$container_workflow" | cut -d: -f1)
login_line=$(grep -n 'uses: docker/login-action' "$container_workflow" | tail -n 1 | cut -d: -f1)
if [ -z "$scan_line" ] || [ -z "$promote_line" ] || [ -z "$login_line" ] \
  || [ "$scan_line" -ge "$promote_line" ] || [ "$promote_line" -ge "$login_line" ]; then
  echo "container workflow must scan local archives before a credentialed promotion login" >&2
  exit 1
fi
printf '%s\n' "$promote_job" | grep -q 'sha256sum -c SHA256SUMS'
printf '%s\n' "$promote_job" | grep -q 'test "${digest}" = "${expected_digest}"'
grep -Fq 'REGISTRY_STORAGE_DELETE_ENABLED=true' "$container_workflow"
cleanup_count=$(grep -Fc 'docker rm --force covalent-private-registry' "$container_workflow")
if [ "$cleanup_count" -lt 2 ]; then
  echo "each private registry stage must have an always cleanup" >&2
  exit 1
fi
grep -Fq -- '--certificate-identity "https://github.com/${GITHUB_REPOSITORY}/.github/workflows/container-supply-chain.yml@refs/tags/${RELEASE_VERSION}"' "$container_workflow"

# GHCR receives only a unique non-consumer candidate before Cosign. Public
# version/latest tags must remain unchanged unless the exact registry digest,
# signing identity, signature, and current SBOM attestation all verify first.
grep -Fq 'candidate="${IMAGE}:candidate-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"' "$container_workflow"
grep -Fq 'certificate_identity="https://github.com/${GITHUB_WORKFLOW_REF}"' "$container_workflow"
grep -Fq 'group: container-release-${{ github.repository_id }}' "$container_workflow"
if grep -Fq 'group: container-release-${{ github.repository_id }}-${{ github.ref }}' "$container_workflow"; then
  echo "container promotion concurrency must serialize every release ref" >&2
  exit 1
fi
grep -Fq 'immutable release tag ${version_ref} already points to a different digest' "$container_workflow"
grep -Fq 'could not prove whether immutable release tag ${version_ref} already exists' "$container_workflow"
test "$(grep -Fc -- '--annotation "index:org.opencontainers.image.version=${{ needs.validate.outputs.image_version }}"' "$container_workflow")" -eq 2
test "$(grep -Fc -- '--annotation "index:io.covalent.source.fingerprint=${{ needs.validate.outputs.source_fingerprint }}"' "$container_workflow")" -eq 2
grep -Fq 'RELEASE_VERSION=${{ needs.validate.outputs.image_version }}' "$container_workflow"
grep -Fq 'source_fingerprint: ${{ steps.docker-source.outputs.fingerprint }}' "$container_workflow"
grep -Fq 'COVALENT_SOURCE_FINGERPRINT=${{ needs.validate.outputs.source_fingerprint }}' "$container_workflow"
grep -Fq 'test "${amd64_fingerprint}" = "${arm64_fingerprint}"' "$container_workflow"
grep -Fq '.annotations["io.covalent.source.fingerprint"] == $fingerprint' "$container_workflow"
grep -Fq 'source-fingerprint: ${{ needs.validate.outputs.source_fingerprint }}' "$container_workflow"
test "$(grep -Fc -- '--build-arg COVALENT_SOURCE_FINGERPRINT=${{ steps.docker-source.outputs.fingerprint }}' .github/workflows/ci.yml)" -eq 2
test "$(grep -Fc -- 'development "${{ steps.docker-source.outputs.fingerprint }}"' .github/workflows/ci.yml)" -eq 2
grep -Fq 'node scripts/container-latest-version.mjs' "$container_workflow"
grep -Fq 'latest digest does not match immutable version provenance ${provenance_ref}' "$container_workflow"
candidate_line=$(grep -n 'Stage the verified index under a non-consumer candidate tag' "$container_workflow" | cut -d: -f1)
sign_line=$(grep -n 'cosign sign --yes "${subject}"' "$container_workflow" | cut -d: -f1)
signature_verify_line=$(grep -n 'cosign verify \\' "$container_workflow" | head -n 1 | cut -d: -f1)
attest_line=$(grep -n 'cosign attest --yes --type spdxjson' "$container_workflow" | head -n 1 | cut -d: -f1)
attestation_verify_line=$(grep -n 'cosign verify-attestation \\' "$container_workflow" | head -n 1 | cut -d: -f1)
public_promote_line=$(grep -n 'Promote the verified signed digest to public release tags' "$container_workflow" | cut -d: -f1)
version_guard_line=$(grep -n 'if existing_digest=$(docker buildx imagetools inspect "${version_ref}"' "$container_workflow" | cut -d: -f1)
latest_guard_line=$(grep -n 'latest_decision=$(node scripts/container-latest-version.mjs' "$container_workflow" | cut -d: -f1)
version_tag_line=$(grep -n 'docker buildx imagetools create --tag "${version_ref}" "${subject}"' "$container_workflow" | cut -d: -f1)
latest_tag_line=$(grep -n 'docker buildx imagetools create --tag "${IMAGE}:latest" "${subject}"' "$container_workflow" | cut -d: -f1)
if [ -z "$candidate_line" ] || [ -z "$sign_line" ] || [ -z "$signature_verify_line" ] \
  || [ -z "$attest_line" ] || [ -z "$attestation_verify_line" ] \
  || [ -z "$public_promote_line" ] || [ -z "$version_guard_line" ] || [ -z "$latest_guard_line" ] \
  || [ -z "$version_tag_line" ] || [ -z "$latest_tag_line" ] \
  || [ "$candidate_line" -ge "$sign_line" ] \
  || [ "$sign_line" -ge "$signature_verify_line" ] \
  || [ "$signature_verify_line" -ge "$attest_line" ] \
  || [ "$attest_line" -ge "$attestation_verify_line" ] \
  || [ "$attestation_verify_line" -ge "$public_promote_line" ] \
  || [ "$public_promote_line" -ge "$version_guard_line" ] \
  || [ "$version_guard_line" -ge "$latest_guard_line" ] \
  || [ "$latest_guard_line" -ge "$version_tag_line" ] \
  || [ "$version_guard_line" -ge "$version_tag_line" ] \
  || [ "$public_promote_line" -ge "$version_tag_line" ] \
  || [ "$public_promote_line" -ge "$latest_tag_line" ]; then
  echo "container public tags must move only after exact signature and SBOM-attestation verification" >&2
  exit 1
fi
test "$(printf '%s\n' "$promote_job" | grep -Fc 'cosign attest --yes --type spdxjson')" -eq 2
printf '%s\n' "$promote_job" | grep -A1 -F -- '--predicate covalent-container-linux-amd64.spdx.json' | grep -Fq '"${amd64_subject}"'
printf '%s\n' "$promote_job" | grep -A1 -F -- '--predicate covalent-container-linux-arm64.spdx.json' | grep -Fq '"${arm64_subject}"'
printf '%s\n' "$promote_job" | grep -Fq 'jq -S -c . "${sbom}" > "${expected}"'
printf '%s\n' "$promote_job" | grep -Fq 'cmp -s "${expected}" "${verified}"'
printf '%s\n' "$promote_job" | grep -Fq "verify_exact_sbom_attestation linux-amd64"
printf '%s\n' "$promote_job" | grep -Fq "verify_exact_sbom_attestation linux-arm64"
printf '%s\n' "$promote_job" | grep -Fq 'linux-amd64-digest: ${IMAGE}@${AMD64_DIGEST}'
printf '%s\n' "$promote_job" | grep -Fq 'linux-arm64-digest: ${IMAGE}@${ARM64_DIGEST}'
printf '%s\n' "$promote_job" | grep -Fq 'verify-linux-amd64-sbom-attestation: cosign verify-attestation --type spdxjson ${IMAGE}@${AMD64_DIGEST}'
printf '%s\n' "$promote_job" | grep -Fq 'verify-linux-arm64-sbom-attestation: cosign verify-attestation --type spdxjson ${IMAGE}@${ARM64_DIGEST}'
printf '%s\n' "$promote_job" | grep -Fq 'covalent-container-linux-amd64.spdx.json'
printf '%s\n' "$promote_job" | grep -Fq 'covalent-container-linux-arm64.spdx.json'
if [ "$(printf '%s\n' "$promote_job" | grep -Fc 'if [[ "${{ needs.validate.outputs.publish_latest }}" == "true" ]]')" -ne 2 ] \
  || printf '%s\n' "$promote_job" | grep -Fq 'version_exists}" == false &&'; then
  echo "container reruns must repair and verify latest after an already-correct immutable version tag" >&2
  exit 1
fi

# Executable fixtures hold the monotonic latest decision to real semantic
# versions and digests, including the one-time trusted v0.1.0 migration.
latest_guard=scripts/container-latest-version.mjs
candidate_digest=sha256:1111111111111111111111111111111111111111111111111111111111111111
current_digest=sha256:2222222222222222222222222222222222222222222222222222222222222222
legacy_digest=sha256:8b8b96bdea7437fecf6d9c3297c248fd9de7eeb25fe7d701aa6f0a5b633cf8a6

newer=$(node "$latest_guard" 0.3.0 "$candidate_digest" 0.2.0 "$current_digest")
test "$(printf '%s\n' "$newer" | jq -r '.action + " " + .currentTag')" = 'promote v0.2.0'
same=$(node "$latest_guard" 0.2.0 "$candidate_digest" 0.2.0 "$candidate_digest")
test "$(printf '%s\n' "$same" | jq -r '.action + " " + .currentTag')" = 'keep v0.2.0'
prerelease=$(node "$latest_guard" 0.3.0-rc.1 "$candidate_digest" absent absent)
test "$(printf '%s\n' "$prerelease" | jq -r '.action')" = skip-prerelease
legacy=$(node "$latest_guard" 0.2.0 "$candidate_digest" unannotated "$legacy_digest")
test "$(printf '%s\n' "$legacy" | jq -r '.action + " " + .currentTag')" = 'promote v0.1.0'

if node "$latest_guard" 0.1.0 "$candidate_digest" 0.2.0 "$current_digest" >/dev/null 2>&1; then
  echo "older stable rerun was allowed to roll latest backward" >&2
  exit 1
fi
if node "$latest_guard" 0.2.0 "$candidate_digest" 0.2.0 "$current_digest" >/dev/null 2>&1; then
  echo "equal version with a different digest was allowed to move latest" >&2
  exit 1
fi
if node "$latest_guard" 0.2.0 "$candidate_digest" unannotated "$current_digest" >/dev/null 2>&1; then
  echo "unannotated latest without trusted legacy provenance was accepted" >&2
  exit 1
fi

# The file-backed Compose secret has no portable uid/gid remapping. Keep the
# service identity fixed and make any common override fail rather than silently
# leaving the owner-only KEK unreadable.
grep -Fq 'user: "65532:65532"' packaging/docker/compose.yaml
grep -Fq 'PUID/PGID overrides are unsupported' packaging/docker/entrypoint.sh
grep -Fq 'PUID override did not fail closed before startup' scripts/check-container-runtime.sh

# The persistent node-side local-api-token is wrapped under the KEK. Release
# harnesses may use client credential files, but no executable script may read
# that record or put a token value into an instrumentation argument.
if rg -n --glob '*.sh' \
  --glob '!check-container-contract.sh' \
  --glob '!test-release-guardrails.sh' \
  '/data/local-api-token' scripts packaging/docker; then
  echo "release harnesses must not read the wrapped node API token" >&2
  exit 1
fi
grep -Fq -- '--api-token-file /run/secrets/covalent-api-token' scripts/check-container-runtime.sh
./scripts/test-container-runtime-fallback.sh
grep -Fq 'command: ["serve", "--api-token-file", "/run/secrets/covalent-api-token"]' packaging/docker/compose.e2e.yaml
grep -Fq 'COVALENT_E2E_TOKEN_A' scripts/docker-compose-e2e.sh
grep -Fq 'COVALENT_PACKAGE_TLS_TOKEN_FILE' scripts/apple-package-tls-e2e.sh
android_tls_harness=scripts/android-api37-device-test.sh
android_tls_test=apps/android/app/src/androidTest/java/life/michaelwong/covalent/CovalentAppTest.kt
grep -Fq -- '-e covalentTlsTokenFile "$tls_token_file_name"' "$android_tls_harness"
if grep -Fq -- '-e covalentTlsToken ' "$android_tls_harness"; then
  echo "Android instrumentation must receive a private token filename, never token bytes" >&2
  exit 1
fi
if grep -Eq '^tls_token=' "$android_tls_harness"; then
  echo "Android TLS harness must not materialize a token value in shell state" >&2
  exit 1
fi
grep -Fq 'shell -T run-as life.michaelwong.covalent' "$android_tls_harness"
grep -Fq 'covalentTlsTokenFile' "$android_tls_test"
grep -Fq 'Os.lstat(tokenFile.absolutePath)' "$android_tls_test"
grep -Fq 'TLS test credential could not be deleted' "$android_tls_test"

# Current native scope is an installable debug-signed Android APK and an ad-hoc
# Apple Silicon app. Production Android signing is deferred, and Apple
# Developer ID/notarization is excluded; neither credentialed workflow is a
# release-foundation dependency.
test -x scripts/build-personal-android-apk.sh
test -x scripts/test-personal-android-apk-builder.sh
./scripts/test-personal-android-apk-builder.sh

apple_unsigned_workflow=.github/workflows/apple-unsigned-release.yml
grep -Fq 'GITHUB_REF}" != "refs/tags/${version}"' "$apple_unsigned_workflow"
grep -Fq 'GITHUB_REF_TYPE}" != "tag"' "$apple_unsigned_workflow"
test -x scripts/install-xcodegen.sh
grep -Fq 'xcodegen_sha256=4d9e34b62172d645eed6457cac13fc222569974098ef4ee9c3368bedf0196806' scripts/install-xcodegen.sh
grep -Fq 'scripts/install-xcodegen.sh' "$apple_unsigned_workflow"
grep -Fq -- '-onlyUsePackageVersionsFromResolvedFile' "$apple_unsigned_workflow"
grep -Fq -- '-packageFingerprintPolicy strict' "$apple_unsigned_workflow"
test -x scripts/build-personal-macos-app.sh
test -x scripts/test-personal-macos-app-builder.sh
./scripts/test-personal-macos-app-builder.sh

# The unsigned Apple lane has no private signing identity, but it still builds
# and verifies with read-only contents before a separate publisher rechecks its
# artifact handoff.
apple_unsigned_sign_job=$(workflow_job "$apple_unsigned_workflow" build-verify)
apple_unsigned_publish_job=$(workflow_job "$apple_unsigned_workflow" publish)
if [ -z "$apple_unsigned_sign_job" ] || [ -z "$apple_unsigned_publish_job" ]; then
  echo "Unsigned Apple release must keep separate build-verify and publish jobs" >&2
  exit 1
fi
if ! printf '%s\n' "$apple_unsigned_sign_job" | grep -q 'contents: read' \
  || printf '%s\n' "$apple_unsigned_sign_job" | grep -q 'contents: write'; then
  echo "Unsigned Apple build job must not have release publication permission" >&2
  exit 1
fi
if ! printf '%s\n' "$apple_unsigned_publish_job" | grep -q 'contents: write'; then
  echo "Unsigned Apple publisher must have the release publication permission" >&2
  exit 1
fi
printf '%s\n' "$apple_unsigned_publish_job" | grep -Fq 'shasum -a 256 -c "${manifest}"'

grep -Fq 'cd "$(dirname "${output}")" && shasum -a 256 "$(basename "${output}")"' "$apple_unsigned_workflow"

grep -q 'must be an annotated tag' scripts/verify-release-commit-signature.sh
grep -q 'git tag -s' scripts/verify-release-commit-signature.sh
grep -q 'historical unsigned annotated tag is grandfathered' scripts/verify-release-commit-signature.sh
grep -q 'No Android artifact is published in v0.1.0' README.md
grep -q 'no active deployable release for the current KEK and trusted-claim' README.md
grep -q 'no verified source-free CLI archive is published yet' README.md
if grep -q 'published releases after v0.1.0 include verified source-free CLI archives' README.md; then
  echo "README must not advertise unpublished CLI archives" >&2
  exit 1
fi
grep -Fq '(docs/getting-started.md)' README.md
grep -Fq '(platform/atlas-tailscale.md)' docs/getting-started.md
grep -q 'No verified CLI archive is published with the historical v0.1.0 release' docs/release/cli-install.md
grep -Fq -- '--certificate-identity "https://github.com/thekozugroup/Covalent/.github/workflows/cli-release.yml@refs/tags/${version}"' docs/release/cli-install.md
# Platform manifests cover more than the end-user archive. The install guide
# must select and validate exactly one archive record instead of asking shasum
# to open SBOM, inventory, and bundle files the reader did not download.
grep -Fq 'awk -v archive="${archive}"' docs/release/cli-install.md
grep -Fq '$2 == archive || $2 == "./" archive' docs/release/cli-install.md
grep -Fq 'test "$(wc -l < "${archive}.sha256" | tr -d '\'' '\'')" = 1' docs/release/cli-install.md
grep -Fq '"${archive}"|"./${archive}")' docs/release/cli-install.md
grep -Fq 'printf '\''%s  %s\n'\'' "${expected_digest}" "${archive}" | shasum -a 256 -c -' docs/release/cli-install.md
if grep -Fq 'shasum -a 256 -c "${manifest}"' docs/release/cli-install.md; then
  echo "CLI install guide must not verify undownloaded manifest assets" >&2
  exit 1
fi

# Android personal users need one complete path from the guarded debug APK
# builder to enrolled CA/token credentials. No Android or browser surface may
# receive the one-time setup code.
test -s docs/platform/android.md
grep -Fq '(docs/platform/android.md)' README.md
grep -Fq '(../../docs/platform/android.md)' apps/android/README.md
grep -Fq '[Android install and onboarding guide](android.md)' docs/platform/atlas-tailscale.md
grep -Fq '[verified APK and onboarding guide](android.md)' docs/platform/unraid.md
grep -Fq './scripts/build-personal-android-apk.sh' docs/platform/android.md
grep -Fq 'app-debug.apk' docs/platform/android.md
grep -Fq 'app-release-unsigned.apk' docs/platform/android.md
grep -Fq '**Choose token file**' docs/platform/android.md
grep -Fq '`root.crt`' docs/platform/android.md
grep -Fq '`local-api-token`' docs/platform/android.md
rg -U -q 'never copy, paste, or type it into[[:space:]]+the[[:space:]]+app' docs/platform/android.md
grep -Fq 'TCP 8443' docs/platform/android.md
grep -Fq 'UDP 8787' docs/platform/android.md
android_strings=apps/android/app/src/main/res/values/strings.xml
grep -Fq 'use the root.crt and access token created by covalent claim on your trusted computer' "$android_strings"
grep -Fq 'this phone never accepts setup codes' "$android_strings"
grep -Fq '<string name="setup_handoff_title">Trusted claim output</string>' "$android_strings"
grep -Fq '<string name="action_choose_token_file">Choose token file</string>' "$android_strings"
grep -Fq 'readClaimTokenFile' apps/android/app/src/main/java/life/michaelwong/covalent/ui/CovalentApp.kt
if rg -q '<string name="(?:setup_handoff|field_node_address)[^"]*">[^<]*covalent://' "$android_strings"; then
  echo "Android setup must not advertise an unproduced setup-link workflow" >&2
  exit 1
fi
grep -Fq '<string name="tailscale_candidate_example">nas.tailnet-name.ts.net:8787</string>' "$android_strings"
grep -Fq 'nas.tailnet-name.ts.net:8787' apps/apple/Sources/CovalentMac/MacDevicesView.swift
grep -Fq 'nas.tailnet-name.ts.net:8787' apps/apple/Sources/CovalentIOS/IOSDevicesView.swift
grep -Fq 'access-token file created by the Covalent claim command on your trusted computer' apps/apple/Sources/CovalentMac/MacSetupViews.swift
if rg -q 'backup server shows this token' apps/apple/Sources/CovalentMac; then
  echo "macOS setup copy must use trusted CLI claim output" >&2
  exit 1
fi
if rg -n 'nas\.tailnet-name\.ts\.net:8788' apps/android apps/apple/Sources/CovalentMac apps/apple/Sources/CovalentIOS docs packaging/docker; then
  echo "user-facing Tailnet peer examples must use UDP 8787" >&2
  exit 1
fi
grep -Fq 'authorized writable destination' apps/android/README.md
grep -Fq 'writable restore destinations' docs/platform/android.md
grep -Fq 'chosen writable destination' docs/platform/unraid.md
grep -Fq 'target inventory and conflict-policy confirmation' apps/apple/README.md
if rg -n 'empty (authorized|restore) destination|create-only signed restores|Apple archives are create-only' \
  apps/android/README.md docs/platform/android.md docs/platform/unraid.md apps/apple/README.md; then
  echo "restore documentation must describe writable targets and conflict policy" >&2
  exit 1
fi
if rg -iq '<string name="caddy_ca_guidance">.*(/config/caddy|copy .* from (the )?container)' "$android_strings" \
  || rg -iq '<string name="node_error_claim_[^"]*">.*(retype|enter|paste).*setup code' "$android_strings"; then
  echo "Android setup copy must use trusted CLI claim output and never solicit a setup code" >&2
  exit 1
fi

# Claim examples cannot rely on a permissive pre-existing home directory or
# output directory. Each guide creates an owner-only parent and setup file, and
# leaves the 0700 output directory for the CLI to create atomically.
for claim_guide in \
  packaging/docker/README.md \
  docs/platform/atlas-tailscale.md \
  docs/platform/unraid.md \
  packaging/unraid/covalent.xml
do
  grep -Fq 'install -d -m 700 "$claim_parent"' "$claim_guide"
  grep -Fq 'install -m 600 /dev/null "$setup_code_file"' "$claim_guide"
  grep -Fq 'test ! -e "$claim_output"' "$claim_guide"
done

# Docker's default bind root is inside the operator home so Docker Desktop
# shares it on macOS. Writable destinations belong to the fixed runtime
# identity, the source remains read/traverse-only, and the privileged read-only
# validator can canonicalize those owner-only paths before startup. Tailnet
# publication binds both protocols to a numeric host address with a numeric
# SocketAddr advertisement.
grep -Fq 'covalent_host_root="$HOME/.covalent-server"' packaging/docker/README.md
grep -Fq 'install -d -o 65532 -g 65532 -m 700 \' packaging/docker/README.md
for safe_directory in config data secrets restore; do
  grep -Fq "\"\$covalent_host_root/$safe_directory\"" packaging/docker/README.md
done
grep -Fq 'install -d -o 65532 -g 65532 -m 500 "$covalent_host_root/source"' packaging/docker/README.md
grep -Fq 'sudo ./scripts/validate-setup-paths.sh \' packaging/docker/README.md
grep -Fq 'export COVALENT_HTTPS_BIND_IP=100.64.0.10' packaging/docker/README.md
grep -Fq 'export COVALENT_PEER_BIND_IP=100.64.0.10' packaging/docker/README.md
grep -Fq 'export COVALENT_ADVERTISED_PEER_ADDRESS=100.64.0.10:8787' packaging/docker/README.md
grep -Fq '"ip": ["tcp:8443", "udp:8787"]' packaging/docker/README.md

# Atlas must either let the entrypoint resolve HTTPS MagicDNS or use a numeric
# SocketAddr. It also pins the SSH host key out of band before remote preflight.
grep -Fq 'Leave `COVALENT_ADVERTISED_PEER_ADDRESS` unset first' docs/platform/atlas-tailscale.md
grep -Fq '`100.64.0.10:8787`' docs/platform/atlas-tailscale.md
if grep -Fq 'COVALENT_ADVERTISED_PEER_ADDRESS=atlas.example-tailnet.ts.net' \
  docs/platform/atlas-tailscale.md packaging/docker/README.md; then
  echo "advertised peer examples must use a numeric SocketAddr, never MagicDNS" >&2
  exit 1
fi
grep -Fq 'ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub' docs/platform/atlas-tailscale.md
grep -Fq 'ssh-keyscan -H -t ed25519 atlas.example-tailnet.ts.net' docs/platform/atlas-tailscale.md
grep -Fq 'if [ "$scanned_fingerprint" != "$trusted_fingerprint" ]; then' docs/platform/atlas-tailscale.md
grep -Fq 'Atlas SSH host-key fingerprint mismatch; refusing to trust it' docs/platform/atlas-tailscale.md
grep -Fq 'StrictHostKeyChecking=yes' docs/platform/atlas-tailscale.md
grep -Fq '"ip": ["tcp:8443", "udp:8787"]' docs/platform/atlas-tailscale.md
# The install flow must name an explicit Tailnet peer endpoint; searching for
# the product contract rather than a whole sentence keeps this static gate from
# drifting when the human-facing explanation changes.
grep -q 'explicit MagicDNS endpoint' docs/platform/atlas-tailscale.md
grep -Fq '## 1. Historical v0.1.0 boundary (do not install)' docs/platform/atlas-tailscale.md
grep -Fq -- '--source /mnt/user/Photos --source /mnt/user/Documents' docs/platform/atlas-tailscale.md
if rg -q -- '--source /srv/' docs/platform/atlas-tailscale.md; then
  echo "Atlas preflight examples must use explicit Unraid share paths" >&2
  exit 1
fi
if rg -n '/data/local-api-token' README.md docs packaging/unraid packaging/docker packaging/web/index.html; then
  echo "active release copy must direct people to trusted claim output, never a wrapped server token path" >&2
  exit 1
fi
if ./scripts/atlas-preflight.sh --ssh operator@atlas.example-tailnet.ts.net --source /mnt/user >/dev/null 2>&1; then
  echo "Atlas preflight must reject a broad Unraid source root" >&2
  exit 1
fi
if ./scripts/atlas-preflight.sh --ssh operator@atlas.example-tailnet.ts.net --source /mnt/user/system >/dev/null 2>&1; then
  echo "Atlas preflight must reject the system share containing the KEK" >&2
  exit 1
fi

echo "release guardrails: ok"
