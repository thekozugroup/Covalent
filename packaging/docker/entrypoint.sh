#!/bin/sh
set -eu

# Containers set UID/GID through Docker's --user option.  The image never starts
# as root, so it cannot silently change ownership of host-mounted paths.
case "${UMASK:-027}" in
  0[0-7][0-7]) umask "$UMASK" ;;
  *) echo "UMASK must be a three-digit octal value such as 027" >&2; exit 64 ;;
esac

for directory in "${COVALENT_CONFIG_DIR:-/config}" "${COVALENT_DATA_DIR:-/data}"; do
  if [ ! -d "$directory" ] || [ ! -w "$directory" ]; then
    echo "Covalent requires a writable durable mount at $directory" >&2
    exit 73
  fi
done

# /config is intentionally durable for operator-managed files such as exported
# safe settings.  The daemon's encrypted state and keys remain together in /data.
if [ "${1:-serve}" != "serve" ]; then
  exec covalent-node "$@"
fi

mkdir -p "${XDG_DATA_HOME:-/config/caddy/data}" "${XDG_CONFIG_HOME:-/config/caddy/config}"

covalent-node "$@" &
node_pid=$!
caddy run --config /etc/caddy/Caddyfile --adapter caddyfile &
proxy_pid=$!

shutdown() {
  trap - INT TERM
  kill -TERM "$proxy_pid" "$node_pid" >/dev/null 2>&1 || true
  wait "$proxy_pid" >/dev/null 2>&1 || true
  wait "$node_pid" >/dev/null 2>&1 || true
  exit 0
}
trap shutdown INT TERM

while kill -0 "$node_pid" >/dev/null 2>&1 && kill -0 "$proxy_pid" >/dev/null 2>&1; do
  sleep 1
done

echo "Covalent node or TLS proxy exited unexpectedly" >&2
kill -TERM "$proxy_pid" "$node_pid" >/dev/null 2>&1 || true
wait "$proxy_pid" >/dev/null 2>&1 || true
wait "$node_pid" >/dev/null 2>&1 || true
exit 1
