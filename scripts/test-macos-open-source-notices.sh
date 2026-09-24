#!/bin/sh
# Focused executable contract for the bounded native macOS notice reader.
set -eu

test "$(uname -s)" = Darwin || { echo "macOS notice reader test requires macOS" >&2; exit 69; }
repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
source_file="$repo_root/apps/apple/Sources/CovalentMac/MacOpenSourceNotices.swift"
root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-notice-reader.XXXXXX")
cleanup() { rm -rf "$root"; }
trap cleanup EXIT HUP INT TERM
mkdir -p "$root/fixture/notices"
printf 'Go go1.26.7 / LICENSE\nfixture notice\n' > "$root/fixture/notices/THIRD-PARTY-NOTICES.txt"
printf '{}\n' > "$root/fixture/notices/manifest.json"

write_index() {
  combined_sha=$(shasum -a 256 "$root/fixture/notices/THIRD-PARTY-NOTICES.txt" | awk '{print $1}')
  combined_bytes=$(wc -c < "$root/fixture/notices/THIRD-PARTY-NOTICES.txt" | tr -d '[:space:]')
  manifest_sha=$(shasum -a 256 "$root/fixture/notices/manifest.json" | awk '{print $1}')
  manifest_bytes=$(wc -c < "$root/fixture/notices/manifest.json" | tr -d '[:space:]')
  cat > "$root/fixture/notices-index.txt" <<EOF
1
combined $combined_sha $combined_bytes notices/THIRD-PARTY-NOTICES.txt
manifest $manifest_sha $manifest_bytes notices/manifest.json
EOF
}
write_index
cat > "$root/Harness.swift" <<'SWIFT'
import Foundation

@main
struct Harness {
    static func main() {
        guard CommandLine.arguments.count == 3 else { exit(64) }
        let root = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        let shouldSucceed = CommandLine.arguments[2] == "success"
        do {
            let value = try MacOpenSourceNotices.load(from: root)
            guard shouldSucceed, value.contains("Go go1.26.7 / LICENSE") else { exit(1) }
        } catch {
            guard !shouldSucceed else { exit(1) }
        }
    }
}
SWIFT
xcrun swiftc -target arm64-apple-macos15.0 "$source_file" "$root/Harness.swift" -o "$root/test-notices"
"$root/test-notices" "$root/fixture" success

printf 'trailing' >> "$root/fixture/notices/THIRD-PARTY-NOTICES.txt"
"$root/test-notices" "$root/fixture" failure
printf 'Go go1.26.7 / LICENSE\nfixture notice\n' > "$root/fixture/notices/THIRD-PARTY-NOTICES.txt"
write_index
mv "$root/fixture/notices/manifest.json" "$root/manifest-real.json"
ln -s "$root/manifest-real.json" "$root/fixture/notices/manifest.json"
"$root/test-notices" "$root/fixture" failure
echo "macOS Open Source Notices reader: ok"
