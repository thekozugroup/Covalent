#!/bin/sh
set -eu

# GitHub CLI's --paginate --slurp emits an array of alert pages. This policy is
# intentionally repository-wide: a commit ref filter can incorrectly hide
# still-open alerts whose original alert instance was on an earlier commit.
alerts=$(jq 'if type == "array" and all(.[]; type == "array") then flatten else . end')

if ! printf '%s\n' "${alerts}" | jq -e 'type == "array" and length == 0' >/dev/null; then
  printf '%s\n' "${alerts}" | jq -r '.[] | "open CodeQL alert #\(.number): \(.rule.id) at \(.most_recent_instance.location.path):\(.most_recent_instance.location.start_line)"' >&2
  exit 1
fi

echo "CodeQL alert policy: zero open alerts."
