#!/bin/sh
set -eu

image="${1:-covalent:foundation}"
container_name="covalent-foundation-check-$$"
started=false

cleanup() {
  if [ "$started" = true ]; then
    docker stop --timeout 10 "$container_name" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT INT TERM

docker run --rm -d \
  --name "$container_name" \
  --read-only \
  --cap-drop=ALL \
  --security-opt=no-new-privileges \
  --tmpfs /tmp:rw,noexec,nosuid,size=64m \
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

runtime=$(docker inspect "$container_name" --format '{{.Config.User}} {{.HostConfig.ReadonlyRootfs}} {{json .HostConfig.CapDrop}} {{json .HostConfig.SecurityOpt}}')
if [ "$runtime" != '65532:65532 true ["ALL"] ["no-new-privileges"]' ]; then
  echo "unexpected container isolation: $runtime" >&2
  exit 1
fi

docker stop --timeout 10 "$container_name" >/dev/null
started=false
echo "rootless read-only container health: ok"
