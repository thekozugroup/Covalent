#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
platform_tool="$repo_root/scripts/container-runtime-platform.sh"
image="${1:-covalent:foundation}"
container_name="covalent-foundation-check-$$"
started=false
network_created=false
client_image_built=false
tls_directory=$(mktemp -d)
kek_directory=$(mktemp -d)
token_directory=$(mktemp -d)
data_volume="$container_name-data"
config_volume="$container_name-config"
network_name="$container_name-network"
client_image="$container_name-cross-arch-client"
runtime_user=65532:65532

host_arch=$(docker info --format '{{.Architecture}}')
image_arch=$(docker image inspect "$image" --format '{{.Architecture}}')
arch_mode=$("$platform_tool" "$host_arch" "$image_arch")

chmod 700 "$kek_directory" "$token_directory"

secret_digest() {
  secret_path=$1
  docker run --rm --user 0:0 --entrypoint sha256sum \
    --mount "type=bind,source=$secret_path,target=/secret,readonly" \
    "$image" /secret | awk '{print $1}'
}

assert_readonly_secret() {
  secret_path=$1
  container_path=$2
  before_digest=$(secret_digest "$secret_path")
  docker run --rm --user "$runtime_user" --entrypoint sh \
    --mount "type=bind,source=$secret_path,target=$container_path,readonly" \
    "$image" -c '
      secret=$1
      test -f "$secret" && test -r "$secret"
      if { printf x >> "$secret"; } 2>/dev/null; then
        echo "readonly secret accepted an append: $secret" >&2
        exit 1
      fi
    ' sh "$container_path"
  after_digest=$(secret_digest "$secret_path")
  if [ "$before_digest" != "$after_digest" ]; then
    echo "readonly secret content changed: $container_path" >&2
    exit 1
  fi
}

cleanup() {
  if [ "$started" = true ]; then
    docker stop --timeout 10 "$container_name" >/dev/null 2>&1 || true
  fi
  if [ "$client_image_built" = true ]; then
    docker image rm "$client_image" >/dev/null 2>&1 || true
  fi
  docker volume rm "$data_volume" "$config_volume" >/dev/null 2>&1 || true
  if [ "$network_created" = true ]; then
    docker network rm "$network_name" >/dev/null 2>&1 || true
  fi
  rm -rf "$tls_directory"
  rm -rf "$kek_directory"
  rm -rf "$token_directory"
}
trap cleanup EXIT INT TERM

docker volume create "$data_volume" >/dev/null
docker volume create "$config_volume" >/dev/null
for key_name in correct wrong; do
  docker run --rm --user "$(id -u):$(id -g)" \
    --mount "type=bind,source=$kek_directory,target=/secrets" \
    "$image" provision-key --key-file "/secrets/$key_name.kek" >/dev/null
  mode=$(stat -f '%Lp' "$kek_directory/$key_name.kek" 2>/dev/null || stat -c '%a' "$kek_directory/$key_name.kek")
  test "$mode" = 600
  # The host-only temporary directory stays 0700 and owned by the caller. Only
  # the individual key is assigned to the immutable runtime UID, so the
  # owner-only file can be mounted directly without a world-writable staging
  # directory or a permissive mode.
  docker run --rm --user 0:0 --entrypoint sh \
    --mount "type=bind,source=$kek_directory/$key_name.kek,target=/secret" \
    "$image" -c 'chown 65532:65532 /secret && chmod 600 /secret'
  assert_readonly_secret "$kek_directory/$key_name.kek" /run/secrets/covalent-kek
done

# Deterministic requests use an explicit caller-owned token file, never the
# wrapped node-side record. Keep a separate client copy before the server copy
# is assigned to the fixed runtime UID.
client_token_file="$tls_directory/client-api-token"
server_token_file="$token_directory/server-api-token"
openssl rand -hex 32 > "$client_token_file"
chmod 600 "$client_token_file"
cp "$client_token_file" "$server_token_file"
chmod 600 "$server_token_file"
docker run --rm --user 0:0 --entrypoint sh \
  --mount "type=bind,source=$server_token_file,target=/secret" \
  "$image" -c 'chown 65532:65532 /secret && chmod 600 /secret'
assert_readonly_secret "$server_token_file" /run/secrets/covalent-api-token

# Docker/Compose's fixed identity is deliberate. Rejecting a common PUID/PGID
# override is safer than silently starting with a UID that cannot read the
# owner-only file-backed secret.
set +e
puid_output=$(docker run --rm --user "$runtime_user" --env PUID=1000 "$image" 2>&1)
puid_status=$?
set -e
if [ "$puid_status" -ne 64 ] || ! printf '%s\n' "$puid_output" | grep -q 'PUID/PGID overrides are unsupported'; then
  echo "PUID override did not fail closed before startup" >&2
  exit 1
fi

# A missing KEK is a locked state, even before any daemon data exists.
set +e
missing_output=$(docker run --rm \
  --user "$runtime_user" \
  --read-only \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --mount "type=volume,source=$data_volume,target=/data" \
  --mount "type=volume,source=$config_volume,target=/config" \
  "$image" 2>&1)
missing_status=$?
set -e
if [ "$missing_status" -ne 78 ] || ! printf '%s\n' "$missing_output" | grep -q 'Covalent is locked'; then
  echo "missing KEK did not fail closed with exit 78" >&2
  exit 1
fi

# Exercise every packaged entrypoint address branch without starting either
# daemon. A valid address reaches the two bounded stubs and then the supervisor
# exits; an invalid SocketAddr must fail with exit 64 before either stub runs.
address_probe_stub="$tls_directory/address-probe-stub"
printf '%s\n' '#!/bin/sh' 'exit 0' > "$address_probe_stub"
chmod 755 "$address_probe_stub"

probe_advertised_address() {
  address=$1
  expected=$2
  label=$3
  set +e
  address_output=$(docker run --rm \
    --user "$runtime_user" \
    --read-only \
    --cap-drop=ALL \
    --security-opt=no-new-privileges \
    --tmpfs /tmp:rw,noexec,nosuid,size=64m \
    --mount "type=volume,source=$data_volume,target=/data" \
    --mount "type=volume,source=$config_volume,target=/config" \
    --mount "type=bind,source=$kek_directory/correct.kek,target=/run/secrets/covalent-kek,readonly" \
    --mount "type=bind,source=$address_probe_stub,target=/usr/local/bin/covalent-node,readonly" \
    --mount "type=bind,source=$address_probe_stub,target=/usr/local/bin/caddy,readonly" \
    --env "COVALENT_ADVERTISED_PEER_ADDRESS=$address" \
    "$image" 2>&1)
  address_status=$?
  set -e
  if [ "$expected" = valid ]; then
    if [ "$address_status" -ne 1 ] \
      || ! printf '%s\n' "$address_output" | grep -q 'Covalent node or TLS proxy exited unexpectedly'; then
      echo "valid advertised $label did not reach packaged daemon startup" >&2
      exit 1
    fi
  elif [ "$address_status" -ne 64 ] \
    || ! printf '%s\n' "$address_output" \
      | grep -q 'COVALENT_ADVERTISED_PEER_ADDRESS must be a numeric IP:port, not a hostname'; then
    echo "invalid advertised $label did not fail closed before startup" >&2
    exit 1
  fi
}

probe_advertised_address atlas.example-tailnet.ts.net:8787 invalid hostname
probe_advertised_address 100.64.0.10:123456 invalid oversized-port
probe_advertised_address '[::1]:8787:123' invalid malformed-suffix
probe_advertised_address '[::::]:8787' invalid malformed-ipv6
probe_advertised_address 100.64.0.10:8787 valid ipv4
probe_advertised_address '[fd7a:115c:a1e0::1]:8787' valid ipv6

docker network create "$network_name" >/dev/null
network_created=true
docker run --rm -d \
  --name "$container_name" \
  --network "$network_name" \
  --network-alias runtime-node \
  --user "$runtime_user" \
  --read-only \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --mount "type=volume,source=$data_volume,target=/data" \
  --mount "type=volume,source=$config_volume,target=/config" \
  --mount "type=bind,source=$kek_directory/correct.kek,target=/run/secrets/covalent-kek,readonly" \
  --mount "type=bind,source=$server_token_file,target=/run/secrets/covalent-api-token,readonly" \
  --publish 127.0.0.1::8443/tcp \
  "$image" serve --api-token-file /run/secrets/covalent-api-token \
  >/dev/null
started=true

attempt=0
until docker exec "$container_name" covalent-node healthcheck --url http://127.0.0.1:8787/healthz >/dev/null 2>&1; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 20 ]; then
    docker logs "$container_name" >&2
    exit 1
  fi
  sleep 1
done
attempt=0
until docker exec "$container_name" test -f /config/caddy/data/caddy/pki/authorities/local/root.crt; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 20 ]; then
    docker logs "$container_name" >&2
    exit 1
  fi
  sleep 1
done
docker cp "$container_name:/config/caddy/data/caddy/pki/authorities/local/root.crt" "$tls_directory/root.crt" >/dev/null

https_port=$(docker port "$container_name" 8443/tcp | sed -n 's/.*://p' | tail -n 1)
if [ -z "$https_port" ]; then
  echo "TLS proxy did not publish a loopback port" >&2
  exit 1
fi
# The Docker publish rule above intentionally binds IPv4 loopback. Keep
# localhost as the TLS hostname/SNI, but pin the connection there as well:
# Docker Desktop can otherwise resolve localhost to ::1 first and never reach
# the published 127.0.0.1 port (notably when exercising linux/amd64 on arm64).
curl_token_config="$tls_directory/api-token.curl"
{
  printf '%s' 'header = "Authorization: Bearer '
  tr -d '\n' < "$client_token_file"
  printf '%s\n' '"'
} > "$curl_token_config"
chmod 600 "$curl_token_config"

set +e
host_tls_output=$(curl --fail --silent --show-error \
  --resolve "localhost:$https_port:127.0.0.1" \
  --cacert "$tls_directory/root.crt" \
  "https://localhost:$https_port/healthz" 2>&1)
host_tls_status=$?
set -e
if [ "$host_tls_status" -eq 0 ]; then
  if curl --fail --silent --show-error --resolve "localhost:$https_port:127.0.0.1" "https://localhost:$https_port/healthz" >/dev/null 2>&1; then
    echo "package CA was unexpectedly trusted without enrollment" >&2
    exit 1
  fi
  if curl --fail --silent --show-error "http://127.0.0.1:$https_port/healthz" >/dev/null 2>&1; then
    echo "management port unexpectedly accepted cleartext HTTP" >&2
    exit 1
  fi
  curl --fail --silent --show-error \
    --resolve "localhost:$https_port:127.0.0.1" \
    --config "$curl_token_config" \
    --cacert "$tls_directory/root.crt" \
    --request POST \
    --header "Accept: application/json" \
    "https://localhost:$https_port/api/v1/config/export" >/dev/null
elif [ "$arch_mode" != cross-arch ]; then
  echo "host TLS health failed for native $image_arch image: $host_tls_output" >&2
  exit 1
else
  if [ "$host_tls_status" -ne 35 ] \
    || ! printf '%s\n' "$host_tls_output" \
      | grep -Eq 'curl: \(35\) LibreSSL/[0-9.]+: .*bad decrypt'; then
    echo "cross-architecture host TLS failed outside the known LibreSSL curl-35 bad-decrypt class: $host_tls_output" >&2
    exit 1
  fi
  command -v nc >/dev/null 2>&1 \
    || { echo "cross-architecture fallback requires nc to verify the published TCP mapping" >&2; exit 1; }
  if ! nc -z -w 3 127.0.0.1 "$https_port"; then
    echo "cross-architecture host TLS fallback requires an open IPv4 publish mapping" >&2
    exit 1
  fi

  # QEMU can corrupt the host LibreSSL handshake for an emulated Caddy binary
  # after TCP connection succeeds. Keep native candidates on host loopback;
  # this bounded branch verifies the same candidate in an isolated network.
  client_context="$tls_directory/cross-arch-client-context"
  mkdir "$client_context"
  docker build --quiet \
    --file - \
    --build-arg "BASE_IMAGE=$image" \
    --tag "$client_image" \
    "$client_context" <<'DOCKERFILE' >/dev/null
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
USER 0:0
RUN apk add --no-cache curl >/dev/null
USER 65532:65532
ENTRYPOINT []
DOCKERFILE
  client_image_built=true
  echo "cross-arch emulation fallback: host LibreSSL curl-35 bad decrypt; verifying $image_arch candidate through isolated Linux client"

  client_curl() {
    docker run --rm \
      --user "$(id -u):$(id -g)" \
      --network "$network_name" \
      --read-only \
      --cap-drop=ALL \
      --security-opt=no-new-privileges \
      --tmpfs /tmp:rw,noexec,nosuid,size=16m \
      --mount "type=bind,source=$tls_directory,target=/client,readonly" \
      --entrypoint /usr/bin/curl \
      "$client_image" "$@"
  }

  if client_curl --fail --silent --show-error \
    --connect-to localhost:8443:runtime-node:8443 \
    "https://localhost:8443/healthz" >/dev/null 2>&1; then
    echo "package CA was unexpectedly trusted without enrollment" >&2
    exit 1
  fi
  if client_curl --fail --silent --show-error \
    --connect-to localhost:8443:runtime-node:8443 \
    "http://localhost:8443/healthz" >/dev/null 2>&1; then
    echo "management port unexpectedly accepted cleartext HTTP" >&2
    exit 1
  fi
  client_curl --fail --silent --show-error \
    --connect-to localhost:8443:runtime-node:8443 \
    --config /client/api-token.curl \
    --cacert /client/root.crt \
    --request POST \
    --header "Accept: application/json" \
    "https://localhost:8443/api/v1/config/export" >/dev/null
fi

runtime=$(docker inspect "$container_name" --format '{{.Config.User}} {{.HostConfig.ReadonlyRootfs}} {{json .HostConfig.CapDrop}} {{json .HostConfig.SecurityOpt}}')
if [ "$runtime" != '65532:65532 true ["ALL"] ["no-new-privileges"]' ]; then
  echo "unexpected container isolation: $runtime" >&2
  exit 1
fi

docker stop --timeout 10 "$container_name" >/dev/null
started=false

# Reopening this exact durable state with a different explicit KEK must fail;
# neither the image nor the node may generate a replacement to make it start.
set +e
wrong_output=$(docker run --rm \
  --user "$runtime_user" \
  --read-only \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --mount "type=volume,source=$data_volume,target=/data" \
  --mount "type=volume,source=$config_volume,target=/config" \
  --mount "type=bind,source=$kek_directory/wrong.kek,target=/run/secrets/covalent-kek,readonly" \
  "$image" 2>&1)
wrong_status=$?
set -e
if [ "$wrong_status" -eq 0 ] || ! printf '%s\n' "$wrong_output" | grep -Eiq 'key|authentication|locked|envelope'; then
  echo "wrong KEK did not refuse existing durable state" >&2
  exit 1
fi

echo "rootless read-only container TLS health: ok"
