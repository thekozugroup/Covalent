#!/bin/sh
set -eu

if [ "$(uname -s)" != "Darwin" ]; then
  echo "Apple package TLS E2E requires macOS Security.framework" >&2
  exit 69
fi
if [ "$#" -ne 4 ]; then
  echo "usage: $0 https://host:port enrolled-root.crt api-token-file wrong-root.crt" >&2
  exit 64
fi

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
base_url=$1
certificate=$2
api_token_file=$3
wrong_certificate=$4

case "$base_url" in
  https://*) ;;
  *) echo "package TLS E2E requires an HTTPS URL" >&2; exit 64 ;;
esac
test -f "$certificate"
test -f "$wrong_certificate"
if [ ! -f "$api_token_file" ] || [ -L "$api_token_file" ]; then
  echo "package TLS E2E requires a regular owner-only API token file" >&2
  exit 64
fi
if ! token_mode=$(stat -c '%a' "$api_token_file" 2>/dev/null || stat -f '%Lp' "$api_token_file" 2>/dev/null); then
  echo "package TLS E2E could not read the API token file mode" >&2
  exit 69
fi
if [ "$token_mode" != 600 ]; then
  echo "package TLS E2E API token file mode is $token_mode; expected 600" >&2
  exit 64
fi

COVALENT_PACKAGE_TLS_BASE_URL="$base_url" \
COVALENT_PACKAGE_TLS_CERTIFICATE="$certificate" \
COVALENT_PACKAGE_TLS_TOKEN_FILE="$api_token_file" \
COVALENT_PACKAGE_TLS_WRONG_CERTIFICATE="$wrong_certificate" \
swift test \
  --package-path "$repo_root/apps/apple" \
  --filter packagedCaddyTLSUsesEnrolledExactCAAndRejectsWrongCA
