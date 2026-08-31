#!/bin/sh
# Static job-boundary contract plus executable CLI attestation fixtures.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
cd "$repo_root"
workflow=.github/workflows/cli-release.yml

workflow_job() {
  awk -v header="  $2:" '
    $0 == header { emit = 1 }
    emit && $0 ~ /^  [A-Za-z0-9_-]+:$/ && $0 != header { exit }
    emit { print }
  ' "$1"
}

build_linux=$(workflow_job "$workflow" build-linux)
build_macos=$(workflow_job "$workflow" build-macos)
sign_job=$(workflow_job "$workflow" sign-cli)
publish_job=$(workflow_job "$workflow" publish)
for job in "$build_linux" "$build_macos" "$sign_job" "$publish_job"; do
  test -n "$job"
done

for build_job in "$build_linux" "$build_macos"; do
  printf '%s\n' "$build_job" | grep -Fq 'contents: read'
  if printf '%s\n' "$build_job" | grep -Eq 'id-token: write|cosign|sign-blob|attest-blob'; then
    echo "CLI build jobs must not hold or invoke keyless signing authority" >&2
    exit 1
  fi
  printf '%s\n' "$build_job" | grep -Fq 'UNSIGNED-SHA256SUMS.txt'
  printf '%s\n' "$build_job" | grep -Fq 'unsigned-covalent-cli-'
done

printf '%s\n' "$sign_job" | grep -Fq 'id-token: write'
printf '%s\n' "$sign_job" | grep -Fq 'needs: [validate, build-linux, build-macos]'
unsigned_checksum_contract="shasum -a 256 -c \"\${unsigned_sums}\""
printf '%s\n' "$sign_job" | grep -Fq "$unsigned_checksum_contract"
printf '%s\n' "$sign_job" | grep -Fq 'cosign sign-blob --yes --bundle'
printf '%s\n' "$sign_job" | grep -Fq 'cosign attest-blob --yes --type spdxjson'
printf '%s\n' "$sign_job" | grep -Fq 'signed-covalent-cli-'
if printf '%s\n' "$sign_job" | grep -Eq 'actions/checkout|rust-toolchain|rust-cache|cargo build|anchore/sbom-action'; then
  echo "CLI signing job must remain a clean handoff verifier, not a build job" >&2
  exit 1
fi
test "$(grep -Fc 'id-token: write' "$workflow")" -eq 1

handoff_verify_line=$(grep -n 'Verify the exact checksummed unsigned handoff' "$workflow" | cut -d: -f1)
sign_line=$(grep -n 'cosign sign-blob --yes --bundle' "$workflow" | cut -d: -f1)
attestation_verify_line=$(grep -n 'verify-cli-release-attestation.sh' "$workflow" | cut -d: -f1)
publish_line=$(grep -n 'publish-release-assets.sh.*release-assets/\*' "$workflow" | cut -d: -f1)
if [ -z "$handoff_verify_line" ] || [ -z "$sign_line" ] \
  || [ -z "$attestation_verify_line" ] || [ -z "$publish_line" ] \
  || [ "$handoff_verify_line" -ge "$sign_line" ] \
  || [ "$sign_line" -ge "$attestation_verify_line" ] \
  || [ "$attestation_verify_line" -ge "$publish_line" ]; then
  echo "CLI release must verify handoff, sign, verify exact attestation, then publish" >&2
  exit 1
fi

printf '%s\n' "$publish_job" | grep -Fq 'needs: [validate, sign-cli]'
printf '%s\n' "$publish_job" | grep -Fq 'pattern: signed-covalent-cli-*'
printf '%s\n' "$publish_job" | grep -Fq '../scripts/verify-cli-release-attestation.sh'
if printf '%s\n' "$publish_job" | grep -Fq 'certificate-identity-regexp'; then
  echo "CLI publication must pin an exact Cosign certificate identity" >&2
  exit 1
fi

test -x scripts/verify-cli-release-attestation.sh
test -x scripts/test-cli-release-attestation.sh
./scripts/test-cli-release-attestation.sh

echo "CLI release workflow fixtures: ok"
