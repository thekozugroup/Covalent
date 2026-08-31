#!/bin/sh
# Deterministic fixture for exact blob-digest and SPDX-predicate verification.
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/covalent-cli-attestation-fixture.XXXXXX")
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT INT TERM

mkdir -p "$fixture/bin"
archive="$fixture/Covalent-v0.2.0-linux-amd64.tar.gz"
sbom="$fixture/Covalent-v0.2.0-linux-amd64-sbom.spdx.json"
bundle="$fixture/Covalent-v0.2.0-linux-amd64.tar.gz.attestation.sigstore.json"
log="$fixture/cosign.log"
identity=https://github.com/thekozugroup/Covalent/.github/workflows/cli-release.yml@refs/tags/v0.2.0
issuer=https://token.actions.githubusercontent.com
printf 'signed CLI archive bytes\n' > "$archive"
printf '%s\n' '{"SPDXID":"SPDXRef-DOCUMENT","name":"Covalent CLI","packages":[{"name":"covalent"}]}' > "$sbom"

cat > "$fixture/bin/cosign" <<'MOCK'
#!/bin/sh
set -eu
: "${COVALENT_FAKE_COSIGN_LOG:?}"
printf '%s\n' "$*" >> "$COVALENT_FAKE_COSIGN_LOG"
[ "${COVALENT_FAKE_COSIGN_FAIL:-false}" = false ] || exit 1
[ "$1" = verify-blob-attestation ]
MOCK
chmod +x "$fixture/bin/cosign"

make_bundle() {
  digest=$1
  predicate=$2
  output=$3
  jq -n --arg digest "$digest" --slurpfile predicate "$predicate" '{
    _type: "https://in-toto.io/Statement/v0.1",
    subject: [{name: "Covalent CLI", digest: {sha256: $digest}}],
    predicateType: "https://spdx.dev/Document",
    predicate: $predicate[0]
  }' > "$fixture/statement.json"
  payload=$(base64 < "$fixture/statement.json" | tr -d '\n')
  jq -n --arg payload "$payload" '{
    mediaType: "application/vnd.dev.sigstore.bundle.v0.3+json",
    dsseEnvelope: {
      payload: $payload,
      payloadType: "application/vnd.in-toto+json",
      signatures: [{sig: "deterministic-fixture"}]
    }
  }' > "$output"
}

archive_digest=$(shasum -a 256 "$archive" | awk '{print $1}')
make_bundle "$archive_digest" "$sbom" "$bundle"
PATH="$fixture/bin:$PATH" COVALENT_FAKE_COSIGN_LOG="$log" \
  "$repo_root/scripts/verify-cli-release-attestation.sh" \
  "$bundle" "$archive" "$sbom" "$identity" "$issuer"
grep -Fq -- '--check-claims=true' "$log"
grep -Fq -- "--certificate-identity $identity" "$log"

jq '.name = "Different SBOM"' "$sbom" > "$fixture/wrong-sbom.json"
if PATH="$fixture/bin:$PATH" COVALENT_FAKE_COSIGN_LOG="$log" \
  "$repo_root/scripts/verify-cli-release-attestation.sh" \
  "$bundle" "$archive" "$fixture/wrong-sbom.json" "$identity" "$issuer" >/dev/null 2>&1; then
  echo "CLI fixture accepted an attestation for a different SBOM" >&2
  exit 1
fi

wrong_digest=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
make_bundle "$wrong_digest" "$sbom" "$fixture/wrong-digest.sigstore.json"
if PATH="$fixture/bin:$PATH" COVALENT_FAKE_COSIGN_LOG="$log" \
  "$repo_root/scripts/verify-cli-release-attestation.sh" \
  "$fixture/wrong-digest.sigstore.json" "$archive" "$sbom" "$identity" "$issuer" >/dev/null 2>&1; then
  echo "CLI fixture accepted an attestation for a different blob digest" >&2
  exit 1
fi

if PATH="$fixture/bin:$PATH" COVALENT_FAKE_COSIGN_LOG="$log" COVALENT_FAKE_COSIGN_FAIL=true \
  "$repo_root/scripts/verify-cli-release-attestation.sh" \
  "$bundle" "$archive" "$sbom" "$identity" "$issuer" >/dev/null 2>&1; then
  echo "CLI fixture ignored cryptographic verification failure" >&2
  exit 1
fi

echo "CLI attestation fixtures: ok"
