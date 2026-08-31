#!/bin/sh
set -eu
umask 077

image=${1:-covalent:foundation}
suffix=$$
node_name="covalent-claim-check-$suffix"
network_name="covalent-claim-check-$suffix"
config_volume="$node_name-config"
data_volume="$node_name-data"
client_image="$node_name-client"
runtime_user=65532:65532
work=$(mktemp -d "${TMPDIR:-/tmp}/covalent-container-claim.XXXXXX")
claim_directory="$work/claim"
kek_directory="$work/kek"
fake_first="$work/fake-first"
fake_replay="$work/fake-replay"
started=false

cleanup() {
  if [ "$started" = true ]; then
    docker stop --timeout 10 "$node_name" >/dev/null 2>&1 || true
  fi
  docker rm --force "$node_name" >/dev/null 2>&1 || true
  docker volume rm "$config_volume" "$data_volume" >/dev/null 2>&1 || true
  docker network rm "$network_name" >/dev/null 2>&1 || true
  docker image rm "$client_image" >/dev/null 2>&1 || true
  chmod -R u+rwx "$work" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT HUP INT TERM

fail() {
  echo "container first-run claim check failed: $1" >&2
  exit 1
}

command -v docker >/dev/null 2>&1 || fail "Docker is required"
command -v jq >/dev/null 2>&1 || fail "jq is required"
docker image inspect "$image" >/dev/null 2>&1 || fail "image is unavailable: $image"

mkdir -p "$claim_directory" "$kek_directory" "$fake_first" "$fake_replay"
chmod 700 "$claim_directory" "$kek_directory"
chmod 755 "$fake_first" "$fake_replay"

# The release image intentionally does not carry a general-purpose HTTP client.
# Derive a transient local-only test client from the exact candidate, adding
# curl solely so the packaged CLI can exercise its real subprocess transport.
client_context="$work/client-context"
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

docker network create "$network_name" >/dev/null
docker volume create "$config_volume" >/dev/null
docker volume create "$data_volume" >/dev/null

docker run --rm --user "$(id -u):$(id -g)" \
  --mount "type=bind,source=$kek_directory,target=/secrets" \
  "$image" provision-key --key-file /secrets/claim.kek >/dev/null
docker run --rm --user 0:0 --entrypoint sh \
  --mount "type=bind,source=$kek_directory/claim.kek,target=/secret" \
  "$image" -c 'chown 65532:65532 /secret && chmod 600 /secret'

start_node() {
  docker run --detach \
    --name "$node_name" \
    --hostname claim-node \
    --network "$network_name" \
    --network-alias claim-node \
    --user "$runtime_user" \
    --read-only \
    --cap-drop=ALL \
    --security-opt=no-new-privileges \
    --tmpfs /tmp:rw,noexec,nosuid,size=64m \
    --env COVALENT_HTTPS_HOST=claim-node \
    --mount "type=volume,source=$config_volume,target=/config" \
    --mount "type=volume,source=$data_volume,target=/data" \
    --mount "type=bind,source=$kek_directory/claim.kek,target=/run/secrets/covalent-kek,readonly" \
    "$image" serve >/dev/null
  started=true
}

wait_for_health() {
  attempt=0
  until docker exec "$node_name" covalent-node healthcheck --url http://127.0.0.1:8787/healthz >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    [ "$attempt" -lt 30 ] || fail "node did not become healthy"
    sleep 1
  done
}

capture_setup_code() {
  log_path="$claim_directory/first-start.log"
  attempt=0
  while :; do
    docker logs "$node_name" >"$log_path" 2>&1
    grep -Eo '[0123456789ABCDEFGHJKMNPQRSTVWXYZ]{5}-[0123456789ABCDEFGHJKMNPQRSTVWXYZ]{5}' \
      "$log_path" | sort -u >"$claim_directory/setup-code"
    if [ "$(wc -l < "$claim_directory/setup-code" | tr -d ' ')" = 1 ]; then
      break
    fi
    attempt=$((attempt + 1))
    [ "$attempt" -lt 30 ] || fail "exactly one setup code was not found in the private startup log"
    sleep 1
  done
  chmod 600 "$claim_directory/setup-code"
  [ "$(tail -c 1 "$claim_directory/setup-code" | od -An -tu1 | tr -d ' ')" = 10 ] \
    || fail "setup-code fixture is not newline-terminated"
}

start_node
wait_for_health
capture_setup_code

cat >"$fake_first/curl" <<'SH'
#!/bin/sh
umask 077
case " $* " in
  *"/api/v1/claim"*)
    /usr/bin/curl --dump-header /claim/first-headers "$@" > /claim/first-response.json
    status=$?
    [ "$status" -eq 0 ] || exit "$status"
    # Model a connection lost after the server committed and returned 200,
    # before the CLI accepted any response bytes.
    exit 56
    ;;
  *) exec /usr/bin/curl "$@" ;;
esac
SH
cat >"$fake_replay/curl" <<'SH'
#!/bin/sh
umask 077
case " $* " in
  *"/api/v1/claim"*)
    /usr/bin/curl --dump-header /claim/replay-headers "$@" > /claim/replay-response.json
    status=$?
    [ "$status" -eq 0 ] || exit "$status"
    cat /claim/replay-response.json
    ;;
  *) exec /usr/bin/curl "$@" ;;
esac
SH
chmod 555 "$fake_first/curl" "$fake_replay/curl"

cli_path=/usr/local/bin/covalent
cli_url=https://claim-node:8443
cli_path_env=/fake-bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
if docker run --rm \
  --user "$(id -u):$(id -g)" \
  --network "$network_name" \
  --env "PATH=$cli_path_env" \
  --mount "type=bind,source=$claim_directory,target=/claim" \
  --mount "type=bind,source=$fake_first,target=/fake-bin,readonly" \
  --entrypoint "$cli_path" \
  "$client_image" claim \
    --https-url "$cli_url" \
    --setup-code-file /claim/setup-code \
    --output-dir /claim/claimed \
  >"$claim_directory/first-cli.out" 2>&1; then
  fail "the deliberately dropped first response unexpectedly succeeded"
fi

grep -Eq '^HTTP/[0-9.]+ 200([[:space:]]|$)' "$claim_directory/first-headers" \
  || fail "the deliberately dropped claim was not an HTTP 200 response"
jq -e '
  (.caCertificate | type == "string" and length > 0) and
  (.caFingerprintSha256 | type == "string" and length == 64) and
  (.sealNonce | type == "string" and length > 0) and
  (.sealedToken | type == "string" and length > 0)
' "$claim_directory/first-response.json" >/dev/null \
  || fail "the dropped response was not a complete successful claim grant"
[ ! -e "$claim_directory/claimed" ] || fail "credentials were published before the response was accepted"
journal_path=$(find "$claim_directory" -maxdepth 1 -type f -name '.covalent-claim-attempt-*' -print)
[ -n "$journal_path" ] || fail "the exact pending request journal was not retained"
[ "$(printf '%s\n' "$journal_path" | wc -l | tr -d ' ')" = 1 ] || fail "more than one claim journal was retained"
[ "$(stat -f '%Lp' "$journal_path" 2>/dev/null || stat -c '%a' "$journal_path")" = 600 ] \
  || fail "claim journal is not owner-only"

docker stop --timeout 10 "$node_name" >/dev/null
docker rm "$node_name" >/dev/null
started=false
start_node
wait_for_health
docker logs "$node_name" >"$claim_directory/restart.log" 2>&1
if grep -Eq '[0123456789ABCDEFGHJKMNPQRSTVWXYZ]{5}-[0123456789ABCDEFGHJKMNPQRSTVWXYZ]{5}' \
  "$claim_directory/restart.log"; then
  fail "a claimed restart minted another setup code"
fi

docker run --rm \
  --user "$(id -u):$(id -g)" \
  --network "$network_name" \
  --env "PATH=$cli_path_env" \
  --mount "type=bind,source=$claim_directory,target=/claim" \
  --mount "type=bind,source=$fake_replay,target=/fake-bin,readonly" \
  --entrypoint "$cli_path" \
  "$client_image" claim \
    --https-url "$cli_url" \
    --setup-code-file /claim/setup-code \
    --output-dir /claim/claimed \
  >"$claim_directory/replay-cli.out" 2>&1 \
  || fail "the exact journal retry did not recover the grant"

grep -Eq '^HTTP/[0-9.]+ 200([[:space:]]|$)' "$claim_directory/replay-headers" \
  || fail "the exact claim replay was not an HTTP 200 response"
cmp -s "$claim_directory/first-response.json" "$claim_directory/replay-response.json" \
  || fail "the replayed grant was not byte-identical after restart"
[ -d "$claim_directory/claimed" ] || fail "verified credentials were not published"
[ "$(stat -f '%Lp' "$claim_directory/claimed" 2>/dev/null || stat -c '%a' "$claim_directory/claimed")" = 700 ] \
  || fail "claim output directory is not owner-only"
for credential in root.crt local-api-token; do
  [ -f "$claim_directory/claimed/$credential" ] || fail "missing claimed credential: $credential"
  [ "$(stat -f '%Lp' "$claim_directory/claimed/$credential" 2>/dev/null || stat -c '%a' "$claim_directory/claimed/$credential")" = 600 ] \
    || fail "claimed credential is not owner-only: $credential"
done
if find "$claim_directory" -maxdepth 1 -type f -name '.covalent-claim-attempt-*' -print -quit | grep -q .; then
  fail "claim journal remained after credentials became durable"
fi

zero32=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
printf '{"clientNonce":"%s","clientProof":"%s"}\n' "$zero32" "$zero32" \
  >"$claim_directory/different-request.json"
docker run --rm \
  --user "$(id -u):$(id -g)" \
  --network "$network_name" \
  --mount "type=bind,source=$claim_directory,target=/claim" \
  --entrypoint /usr/bin/curl \
  "$client_image" --silent --show-error --insecure \
    --request POST --header 'Content-Type: application/json' \
    --data-binary @/claim/different-request.json \
    --output /dev/null --write-out '%{http_code}' \
    "$cli_url/api/v1/claim" >"$claim_directory/different-status"
[ "$(cat "$claim_directory/different-status")" = 409 ] \
  || fail "a different request did not remain closed with HTTP 409"

{
  printf '%s' 'header = "Authorization: Bearer '
  tr -d '\n' <"$claim_directory/claimed/local-api-token"
  printf '%s\n' '"'
} >"$claim_directory/auth.curl"
chmod 600 "$claim_directory/auth.curl"
docker run --rm \
  --user "$(id -u):$(id -g)" \
  --network "$network_name" \
  --mount "type=bind,source=$claim_directory,target=/claim,readonly" \
  --entrypoint /usr/bin/curl \
  "$client_image" --fail --silent --show-error \
    --config /claim/auth.curl \
    --cacert /claim/claimed/root.crt \
    --request POST \
    "$cli_url/api/v1/config/export" >/dev/null \
  || fail "claimed CA, exact hostname, and token did not authenticate"

if docker run --rm \
  --user "$(id -u):$(id -g)" \
  --network "$network_name" \
  --entrypoint /usr/bin/curl \
  "$client_image" --fail --silent --show-error "$cli_url/healthz" \
  >"$claim_directory/untrusted.out" 2>&1; then
  fail "the private CA was trusted without enrollment"
fi

node_ip=$(docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$node_name")
if docker run --rm \
  --user "$(id -u):$(id -g)" \
  --network "$network_name" \
  --mount "type=bind,source=$claim_directory,target=/claim,readonly" \
  --entrypoint /usr/bin/curl \
  "$client_image" --fail --silent --show-error \
    --cacert /claim/claimed/root.crt "https://$node_ip:8443/healthz" \
  >"$claim_directory/wrong-host.out" 2>&1; then
  fail "the claimed CA accepted the wrong hostname"
fi

echo "packaged first-run claim loss/restart/replay/TLS contract: ok"
