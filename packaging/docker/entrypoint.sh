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
exec covalent-node "$@"
