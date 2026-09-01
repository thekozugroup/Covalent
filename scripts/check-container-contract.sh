#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
dockerfile="$repo_root/packaging/docker/Dockerfile"
documentation="$repo_root/packaging/docker/README.md"
entrypoint="$repo_root/packaging/docker/entrypoint.sh"
compose="$repo_root/packaging/docker/compose.yaml"
e2e_compose="$repo_root/packaging/docker/compose.e2e.yaml"
e2e_script="$repo_root/scripts/docker-compose-e2e.sh"
runtime_script="$repo_root/scripts/check-container-runtime.sh"
claim_script="$repo_root/scripts/check-container-claim.sh"
apple_tls_script="$repo_root/scripts/apple-package-tls-e2e.sh"
caddy_gomod="$repo_root/packaging/docker/caddy/go.mod"
caddy_gosum="$repo_root/packaging/docker/caddy/go.sum"
runtime_digest="sha256:fd791d74b68913cbb027c6546007b3f0d3bc45125f797758156952bc2d6daf40"

require_text() {
  expected=$1
  path=$2
  if ! grep -Fq -- "$expected" "$path"; then
    echo "container contract is missing '$expected' in $path" >&2
    exit 1
  fi
}

require_text "FROM rust:1.97.1-alpine3.23@sha256:" "$dockerfile"
require_text "ENV CARGO_INCREMENTAL=0" "$dockerfile"
require_text "FROM alpine:3.23@$runtime_digest" "$dockerfile"

# Caddy is built from source against a pinned Go toolchain rather than lifted
# out of caddy:2.11.4-alpine, whose published binary is linked against go1.26.3.
# The source is the pinned upstream compatibility snapshot
# v2.11.5-0.20260711231708-b2693fb63a30 (33 commits after v2.11.4), not a
# v2.11.4 runtime. The full delta is recorded in the security analysis.
# The toolchain, snapshot, and module bumps are pinned by digest or exact
# version, and go.sum must exist or -mod=readonly has nothing to verify.
require_text "FROM golang:1.26.7-alpine3.23@sha256:" "$dockerfile"
require_text "GOTOOLCHAIN=local" "$dockerfile"
require_text "GOFLAGS=-mod=readonly" "$dockerfile"
require_text "github.com/caddyserver/caddy/v2 v2.11.5-0.20260711231708-b2693fb63a30" "$caddy_gomod"
# Eight stdlib advisories are fixed in go1.26.6; GOTOOLCHAIN=local makes this
# directive the floor the compiler itself enforces. Lowering it re-opens them.
require_text "go 1.26.7" "$caddy_gomod"
require_text "google.golang.org/grpc v1.82.1" "$caddy_gomod"
require_text "golang.org/x/text v0.39.0" "$caddy_gomod"
require_text "go.opentelemetry.io/otel v1.44.0" "$caddy_gomod"
require_text "github.com/google/cel-go v0.30.0" "$caddy_gomod"
require_text "github.com/go-chi/chi/v5 v5.3.0" "$caddy_gomod"
require_text "github.com/klauspost/compress v1.18.7" "$caddy_gomod"
require_text "cel-go drifted from reviewed v0.30.0" "$dockerfile"
require_text "go-chi drifted from reviewed v5.3.0" "$dockerfile"
require_text "klauspost/compress drifted from reviewed v1.18.7" "$dockerfile"
if [ ! -s "$caddy_gosum" ]; then
  echo "container contract requires a non-empty $caddy_gosum for -mod=readonly" >&2
  exit 1
fi
require_text "org.opencontainers.image.base.name=\"docker.io/library/alpine:3.23\"" "$dockerfile"
require_text "org.opencontainers.image.base.digest=\"$runtime_digest\"" "$dockerfile"
require_text 'ARG RELEASE_VERSION=development' "$dockerfile"
require_text 'org.opencontainers.image.version="$RELEASE_VERSION"' "$dockerfile"
require_text 'ARG COVALENT_SOURCE_FINGERPRINT=unknown' "$dockerfile"
require_text 'io.covalent.source.fingerprint="$COVALENT_SOURCE_FINGERPRINT"' "$dockerfile"
require_text 'io.covalent.runtime.openssl.version="3.5.8-r0"' "$dockerfile"
require_text 'libcrypto3=3.5.8-r0' "$dockerfile"
require_text 'libssl3=3.5.8-r0' "$dockerfile"
require_text 'Alpine 3.23 runtime base' "$documentation"
require_text 'OpenSSL libraries are upgraded to the exact signed Alpine security revision `3.5.8-r0`' "$documentation"
# Markdown backticks are literal documentation text.
# shellcheck disable=SC2016
require_text '`linux/amd64` and `linux/arm64`' "$documentation"
require_text 'COVALENT_KEY_ENCRYPTION_KEY_FILE:=/run/secrets/covalent-kek' "$entrypoint"
require_text 'Covalent never generates a replacement KEK' "$entrypoint"
require_text 'COVALENT_KEY_ENCRYPTION_KEY_FILE: /run/secrets/covalent-kek' "$compose"
require_text 'COVALENT_ADVERTISED_PEER_ADDRESS: "${COVALENT_ADVERTISED_PEER_ADDRESS:-}"' "$compose"
require_text '${COVALENT_HTTPS_BIND_IP:-127.0.0.1}:${COVALENT_HTTPS_PORT:-8443}:8443/tcp' "$compose"
require_text '${COVALENT_PEER_BIND_IP:-0.0.0.0}:${COVALENT_PEER_PORT:-8787}:8787/udp' "$compose"
require_text 'source: covalent-kek' "$compose"
require_text 'COVALENT_KEK_FILE' "$compose"
require_text 'user: "65532:65532"' "$compose"
require_text 'uid: "65532"' "$compose"
require_text 'gid: "65532"' "$compose"
require_text 'PUID/PGID overrides are unsupported' "$entrypoint"
require_text 'unset COVALENT_ADVERTISED_PEER_ADDRESS' "$entrypoint"
require_text 'COVALENT_ADVERTISED_PEER_ADDRESS must be a numeric IP:port, not a hostname' "$entrypoint"
require_text 'function count_groups(text, groups, count, position)' "$entrypoint"
require_text 'probe_advertised_address atlas.example-tailnet.ts.net:8787 invalid hostname' "$runtime_script"
require_text 'probe_advertised_address 100.64.0.10:123456 invalid oversized-port' "$runtime_script"
require_text "probe_advertised_address '[::1]:8787:123' invalid malformed-suffix" "$runtime_script"
require_text "probe_advertised_address '[::::]:8787' invalid malformed-ipv6" "$runtime_script"
require_text 'probe_advertised_address 100.64.0.10:8787 valid ipv4' "$runtime_script"
require_text "probe_advertised_address '[fd7a:115c:a1e0::1]:8787' valid ipv6" "$runtime_script"
require_text 'covalent_host_root="$HOME/.covalent-server"' "$documentation"
require_text 'install -d -o 65532 -g 65532 -m 700 \' "$documentation"
for safe_directory in config data secrets restore; do
  require_text "\"\$covalent_host_root/$safe_directory\"" "$documentation"
done
require_text 'install -d -o 65532 -g 65532 -m 500 "$covalent_host_root/source"' "$documentation"
require_text 'sudo ./scripts/validate-setup-paths.sh \' "$documentation"
require_text 'install -d -m 700 "$claim_parent"' "$documentation"
require_text 'install -m 600 /dev/null "$setup_code_file"' "$documentation"
require_text 'test ! -e "$claim_output"' "$documentation"
require_text 'export COVALENT_HTTPS_BIND_IP=100.64.0.10' "$documentation"
require_text 'export COVALENT_PEER_BIND_IP=100.64.0.10' "$documentation"
require_text 'export COVALENT_ADVERTISED_PEER_ADDRESS=100.64.0.10:8787' "$documentation"
require_text 'COVALENT_E2E_KEK_A' "$e2e_compose"
require_text 'COVALENT_E2E_KEK_B' "$e2e_compose"
require_text 'COVALENT_E2E_KEK_C' "$e2e_compose"
require_text 'COVALENT_E2E_TOKEN_A' "$e2e_compose"
require_text 'COVALENT_E2E_TOKEN_B' "$e2e_compose"
require_text 'COVALENT_E2E_TOKEN_C' "$e2e_compose"
require_text 'command: ["serve", "--api-token-file", "/run/secrets/covalent-api-token"]' "$e2e_compose"
require_text 'provision-key --key-file "/secrets/$node.kek"' "$e2e_script"
require_text 'openssl rand -hex 32 > "$server_token_path"' "$e2e_script"
require_text 'missing KEK did not fail closed with exit 78' "$runtime_script"
require_text 'wrong KEK did not refuse existing durable state' "$runtime_script"
require_text 'PUID override did not fail closed before startup' "$runtime_script"
require_text 'assert_readonly_secret "$kek_directory/$key_name.kek" /run/secrets/covalent-kek' "$runtime_script"
require_text 'assert_readonly_secret "$server_token_file" /run/secrets/covalent-api-token' "$runtime_script"
require_text 'if { printf x >> "$secret"; } 2>/dev/null; then' "$runtime_script"
require_text 'before_digest=$(secret_digest "$secret_path")' "$runtime_script"
require_text 'after_digest=$(secret_digest "$secret_path")' "$runtime_script"
require_text 'if { printf x >> /secret; } 2>/dev/null; then' "$e2e_script"
require_text 'before_digest=$(docker run --rm --user 0:0 --entrypoint sha256sum' "$e2e_script"
require_text 'after_digest=$(docker run --rm --user 0:0 --entrypoint sha256sum' "$e2e_script"
require_text '--api-token-file /run/secrets/covalent-api-token' "$runtime_script"

# GNU stat -f treats its following format as another path and can emit
# filesystem details before failing. If its BSD form runs first, the fallback
# mode is appended to that output and otherwise-correct permission checks fail.
for portable_stat_script in "$runtime_script" "$e2e_script" "$claim_script" "$apple_tls_script"; do
  if grep -Eq 'stat[[:space:]]+-f[^|]*\|\|[[:space:]]*stat[[:space:]]+-c' "$portable_stat_script"; then
    echo "packaged TLS/container harness must try GNU stat -c before BSD stat -f: $portable_stat_script" >&2
    exit 1
  fi
done

[ -x "$claim_script" ] || {
  echo "packaged claim replay harness is missing or not executable: $claim_script" >&2
  exit 1
}
require_text 'setup-code fixture is not newline-terminated' "$claim_script"
require_text 'the deliberately dropped first response unexpectedly succeeded' "$claim_script"
require_text 'the deliberately dropped claim was not an HTTP 200 response' "$claim_script"
require_text 'the replayed grant was not byte-identical after restart' "$claim_script"
require_text 'claim journal remained after credentials became durable' "$claim_script"
require_text 'a different request did not remain closed with HTTP 409' "$claim_script"
require_text 'claimed CA, exact hostname, and token did not authenticate' "$claim_script"
require_text 'the claimed CA accepted the wrong hostname' "$claim_script"

# The node-side local-api-token is an encrypted durable record, not a client
# credential. Deterministic harnesses must inject a separate caller-owned token
# file and must never recover or forward this wrapped record.
token_reference_scan_status=0
if command -v rg >/dev/null 2>&1; then
  token_reference_hits=$(rg -n --glob '*.sh' \
    --glob '!check-container-contract.sh' \
    --glob '!test-release-guardrails.sh' \
    '/data/local-api-token' "$repo_root/scripts" "$repo_root/packaging/docker") || \
    token_reference_scan_status=$?
else
  token_reference_hits=$(find "$repo_root/scripts" "$repo_root/packaging/docker" \
    -type f -name '*.sh' \
    ! -name 'check-container-contract.sh' \
    ! -name 'test-release-guardrails.sh' \
    -exec grep -nH -F -- '/data/local-api-token' {} \;) || \
    token_reference_scan_status=$?
fi
if [ "$token_reference_scan_status" -gt 1 ]; then
  echo "could not scan container harnesses for wrapped token access" >&2
  exit 1
fi
if [ -n "$token_reference_hits" ]; then
  printf '%s\n' "$token_reference_hits"
  echo "executable container harnesses must not read the wrapped node API token" >&2
  exit 1
fi

if grep -Eiq 'tailscale\.sock|docker\.sock' "$compose"; then
  echo "container compose must not mount a Docker or Tailscale socket" >&2
  exit 1
fi

if grep -Eiq '(runtime|base image).*(Debian|Bookworm|glibc)|(Debian|Bookworm|glibc).*(runtime|base image)' "$documentation"; then
  echo "container documentation describes an obsolete non-Alpine runtime" >&2
  exit 1
fi

image=${1:-}
expected_revision=${2:-}
expected_version=${3:-}
expected_source_fingerprint=${4:-}
if [ -n "$image" ]; then
  command -v docker >/dev/null 2>&1 || {
    echo "Docker is required to inspect $image" >&2
    exit 1
  }
  test "$(docker image inspect "$image" --format '{{.Os}}')" = linux
  test "$(docker image inspect "$image" --format '{{.Config.User}}')" = 65532:65532
  test "$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.base.name"}}')" = docker.io/library/alpine:3.23
  test "$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.base.digest"}}')" = "$runtime_digest"
  test "$(docker image inspect "$image" --format '{{index .Config.Labels "io.covalent.runtime.openssl.version"}}')" = 3.5.8-r0
  test "$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.licenses"}}')" = MIT
  actual_version=$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.version"}}')
  [ -n "$actual_version" ] || { echo "container OCI version label is empty" >&2; exit 1; }
  actual_source_fingerprint=$(docker image inspect "$image" --format '{{index .Config.Labels "io.covalent.source.fingerprint"}}')
  if ! printf '%s\n' "$actual_source_fingerprint" | grep -Eq '^[0-9a-f]{64}$'; then
    echo "container source fingerprint label must be an exact non-unknown SHA-256 digest" >&2
    exit 1
  fi
  docker run --rm --entrypoint sh "$image" -c \
    'apk info -e "libcrypto3=3.5.8-r0" && apk info -e "libssl3=3.5.8-r0"' \
    >/dev/null
  # The Dockerfile asserts the build inputs; this asserts the artefact. A
  # from-source Caddy that silently drifted from the reviewed upstream
  # compatibility snapshot would still produce a green build without it.
  caddy_version=$(docker run --rm --entrypoint caddy "$image" version 2>/dev/null | awk '{print $1}')
  if [ "$caddy_version" != "v2.11.5-0.20260711231708-b2693fb63a30" ]; then
    echo "packaged Caddy reports '$caddy_version', expected v2.11.5-0.20260711231708-b2693fb63a30" >&2
    exit 1
  fi
  if [ -n "$expected_revision" ]; then
    actual_revision=$(docker image inspect "$image" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}')
    if [ "$actual_revision" != "$expected_revision" ]; then
      echo "container OCI revision mismatch: expected $expected_revision, got $actual_revision" >&2
      exit 1
    fi
  fi
  if [ -n "$expected_version" ] && [ "$actual_version" != "$expected_version" ]; then
    echo "container OCI version mismatch: expected $expected_version, got $actual_version" >&2
    exit 1
  fi
  if [ -n "$expected_source_fingerprint" ]; then
    if ! printf '%s\n' "$expected_source_fingerprint" | grep -Eq '^[0-9a-f]{64}$'; then
      echo "expected container source fingerprint is not a SHA-256 digest" >&2
      exit 1
    fi
    if [ "$actual_source_fingerprint" != "$expected_source_fingerprint" ]; then
      echo "container source fingerprint mismatch: expected $expected_source_fingerprint, got $actual_source_fingerprint" >&2
      exit 1
    fi
  fi
fi

echo "container Alpine documentation and OCI contract: ok"
