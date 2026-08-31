#!/bin/sh
# Cryptographically verify one CLI blob attestation, then prove that its signed
# statement names this exact archive digest and embeds this exact SPDX SBOM.
set -eu

if [ "$#" -ne 5 ]; then
  echo "usage: verify-cli-release-attestation.sh BUNDLE ARCHIVE SBOM IDENTITY ISSUER" >&2
  exit 1
fi
bundle=${1:?usage: verify-cli-release-attestation.sh BUNDLE ARCHIVE SBOM IDENTITY ISSUER}
archive=${2:?usage: verify-cli-release-attestation.sh BUNDLE ARCHIVE SBOM IDENTITY ISSUER}
sbom=${3:?usage: verify-cli-release-attestation.sh BUNDLE ARCHIVE SBOM IDENTITY ISSUER}
certificate_identity=${4:?usage: verify-cli-release-attestation.sh BUNDLE ARCHIVE SBOM IDENTITY ISSUER}
certificate_issuer=${5:?usage: verify-cli-release-attestation.sh BUNDLE ARCHIVE SBOM IDENTITY ISSUER}

for path in "$bundle" "$archive" "$sbom"; do
  [ -f "$path" ] || {
    echo "CLI attestation input is missing: $path" >&2
    exit 1
  }
done

# This verifies the DSSE signature, Fulcio identity, Rekor evidence, predicate
# type, and the blob claim before any JSON is trusted below.
cosign verify-blob-attestation \
  --bundle "$bundle" \
  --type spdxjson \
  --check-claims=true \
  --certificate-identity "$certificate_identity" \
  --certificate-oidc-issuer "$certificate_issuer" \
  "$archive" >/dev/null

work=$(mktemp -d "${TMPDIR:-/tmp}/covalent-cli-attestation.XXXXXX")
cleanup() { rm -rf "$work"; }
trap cleanup EXIT INT TERM

jq -e '
  (.mediaType | type == "string" and startswith("application/vnd.dev.sigstore.bundle")) and
  .dsseEnvelope.payloadType == "application/vnd.in-toto+json" and
  (.dsseEnvelope.signatures | type == "array" and length == 1)
' "$bundle" >/dev/null
payload=$(jq -er '.dsseEnvelope.payload | select(type == "string" and length > 0)' "$bundle")
printf '%s' "$payload" | base64 --decode > "$work/statement.json"

archive_digest=$(shasum -a 256 "$archive" | awk '{print $1}')
case "$archive_digest" in
  ''|*[!0-9a-f]*)
    echo "could not derive the CLI archive SHA-256 digest" >&2
    exit 1
    ;;
esac
test "${#archive_digest}" -eq 64

jq -e --arg digest "$archive_digest" '
  .predicateType == "https://spdx.dev/Document" and
  (.subject | type == "array" and length == 1) and
  (.subject[0].digest | type == "object" and keys == ["sha256"]) and
  .subject[0].digest.sha256 == $digest and
  (.predicate | type == "object")
' "$work/statement.json" >/dev/null

jq -S -c . "$sbom" > "$work/expected-sbom.json"
jq -S -c .predicate "$work/statement.json" > "$work/attested-sbom.json"
if ! cmp -s "$work/expected-sbom.json" "$work/attested-sbom.json"; then
  echo "verified CLI attestation predicate does not equal the published SBOM" >&2
  exit 1
fi
