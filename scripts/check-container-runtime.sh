#!/bin/sh
set -eu

image="${1:-covalent:foundation}"
container_name="covalent-foundation-check-$$"
started=false
tls_directory=$(mktemp -d)

cleanup() {
  if [ "$started" = true ]; then
    docker stop --timeout 10 "$container_name" >/dev/null 2>&1 || true
  fi
  rm -rf "$tls_directory"
}
trap cleanup EXIT INT TERM

docker run --rm -d \
  --name "$container_name" \
  --read-only \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,size=64m \
  --publish 127.0.0.1::8443/tcp \
  "$image" \
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
attempt=0
until curl --fail --silent --show-error --cacert "$tls_directory/root.crt" "https://localhost:$https_port/healthz" >/dev/null 2>&1; do
  attempt=$((attempt + 1))
  if [ "$attempt" -ge 20 ]; then
    docker logs "$container_name" >&2
    exit 1
  fi
  sleep 1
done
if curl --fail --silent --show-error "https://localhost:$https_port/healthz" >/dev/null 2>&1; then
  echo "package CA was unexpectedly trusted without enrollment" >&2
  exit 1
fi
if curl --fail --silent --show-error "http://127.0.0.1:$https_port/healthz" >/dev/null 2>&1; then
  echo "management port unexpectedly accepted cleartext HTTP" >&2
  exit 1
fi
api_token=$(docker exec "$container_name" sh -c 'cat /data/local-api-token')
curl --fail --silent --show-error \
  --cacert "$tls_directory/root.crt" \
  --request POST \
  --header "Authorization: Bearer $api_token" \
  --header "Accept: application/json" \
  "https://localhost:$https_port/api/v1/config/export" >/dev/null

runtime=$(docker inspect "$container_name" --format '{{.Config.User}} {{.HostConfig.ReadonlyRootfs}} {{json .HostConfig.CapDrop}} {{json .HostConfig.SecurityOpt}}')
if [ "$runtime" != '65532:65532 true ["ALL"] ["no-new-privileges"]' ]; then
  echo "unexpected container isolation: $runtime" >&2
  exit 1
fi

docker stop --timeout 10 "$container_name" >/dev/null
started=false
echo "rootless read-only container TLS health: ok"
