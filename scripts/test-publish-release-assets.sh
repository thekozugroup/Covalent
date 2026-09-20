#!/bin/sh
# Deterministic gh fixture for draft discovery and numeric-ID asset upload.
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
fixture=$(mktemp -d "${TMPDIR:-/tmp}/covalent-release-publisher.XXXXXX")
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT INT TERM

mkdir -p "$fixture/bin"
log="$fixture/gh.log"
curl_log="$fixture/curl.log"
asset="$fixture/Covalent-v0.2.0-test.txt"
printf 'verified release bytes\n' > "$asset"

cat > "$fixture/bin/gh" <<'MOCK'
#!/bin/sh
set -eu
: "${COVALENT_FAKE_GH_LOG:?}"
printf '%s\n' "$*" >> "$COVALENT_FAKE_GH_LOG"

case "$*" in
  *"repos/thekozugroup/Covalent/releases?per_page=100"*)
    # Existing draft: tag endpoints may not expose it, authenticated listing does.
    printf '[[{"id":424,"draft":true,"tag_name":"v0.2.0","upload_url":"https://uploads.github.com/repos/thekozugroup/Covalent/releases/424/assets{?name,label}"}]]\n'
    ;;
  *"--method DELETE repos/thekozugroup/Covalent/releases/assets/900 --silent"*)
    ;;
  *"repos/thekozugroup/Covalent/releases/424/assets?per_page=100"*)
    case "$*" in
      *--slurp*) printf '[[{"id":900,"name":"Covalent-v0.2.0-test.txt","size":23}]]\n' ;;
      *) printf '[{"id":900,"name":"Covalent-v0.2.0-test.txt","size":23}]\n' ;;
    esac
    ;;
  *)
    echo "unexpected gh fixture call: $*" >&2
    exit 1
    ;;
esac
MOCK
chmod +x "$fixture/bin/gh"

cat > "$fixture/bin/curl" <<'MOCK'
#!/bin/sh
set -eu
: "${COVALENT_FAKE_CURL_LOG:?}"
printf '%s\n' "$*" >> "$COVALENT_FAKE_CURL_LOG"

input=''
previous=''
for argument in "$@"; do
  if [ "$previous" = --data-binary ]; then input=${argument#@}; fi
  previous=$argument
done
test "$(cat "$input")" = 'verified release bytes'
case "$*" in
  *"https://uploads.github.com/repos/thekozugroup/Covalent/releases/424/assets?name=Covalent-v0.2.0-test.txt"*) ;;
  *)
    echo "upload fixture did not use the release upload URL" >&2
    exit 1
    ;;
esac
MOCK
chmod +x "$fixture/bin/curl"

output=$(PATH="$fixture/bin:$PATH" \
  COVALENT_FAKE_GH_LOG="$log" \
  COVALENT_FAKE_CURL_LOG="$curl_log" \
  GH_TOKEN=fake \
  GITHUB_REPOSITORY=thekozugroup/Covalent \
  "$repo_root/scripts/publish-release-assets.sh" v0.2.0 "$asset")

printf '%s\n' "$output" | grep -Fq 'uploaded Covalent-v0.2.0-test.txt'
grep -Fq 'repos/thekozugroup/Covalent/releases?per_page=100' "$log"
grep -Fq -- '--method DELETE repos/thekozugroup/Covalent/releases/assets/900 --silent' "$log"
grep -Fq 'https://uploads.github.com/repos/thekozugroup/Covalent/releases/424/assets?name=Covalent-v0.2.0-test.txt' "$curl_log"
if grep -E -- '--slurp.*(--jq|--template)|(--jq|--template).*--slurp' "$log"; then
  echo "gh fixture saw an unsupported --slurp/--jq or --template combination" >&2
  exit 1
fi
if grep -Eq '^release (view|create|upload)' "$log"; then
  echo "draft fixture fell back to a tag-addressed release operation" >&2
  exit 1
fi

echo "draft release publisher fixture: ok"
