#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
builder="$repo_root/scripts/build-android-jni.sh"
fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/covalent-android-jni-command.XXXXXX")
cleanup() {
  rm -rf "$fixture_root"
}
trap cleanup EXIT INT TERM

fake_bin="$fixture_root/bin"
fake_ndk="$fixture_root/ndk/27.1.12297006"
cargo_log="$fixture_root/cargo-arguments"
mkdir -p "$fake_bin" "$fake_ndk"

cat > "$fake_bin/rustup" <<'EOF'
#!/bin/sh
set -eu
case "${1:-} ${2:-}" in
  'which cargo') printf '%s\n' "$COVALENT_TEST_CARGO_BIN" ;;
  'which rustc') printf '%s\n' /usr/bin/true ;;
  'target list') printf '%s\n' aarch64-linux-android x86_64-linux-android ;;
  *) echo "unexpected rustup arguments: $*" >&2; exit 90 ;;
esac
EOF

cat > "$fake_bin/cargo" <<'EOF'
#!/bin/sh
set -eu
if [ "$#" -eq 2 ] && [ "$1" = ndk ] && [ "$2" = --version ]; then
  echo 'cargo-ndk 4.1.2'
  exit 0
fi
printf '%s\n' "$@" > "$COVALENT_TEST_CARGO_LOG"
# Stop after observing the first ABI's command. The builder must preserve this
# sentinel status instead of reaching the linker with a nonexistent archive.
exit 73
EOF
chmod +x "$fake_bin/rustup" "$fake_bin/cargo"

status=0
PATH="$fake_bin:$PATH" \
  COVALENT_TEST_CARGO_BIN="$fake_bin/cargo" \
  COVALENT_TEST_CARGO_LOG="$cargo_log" \
  COVALENT_ANDROID_NDK_HOME="$fake_ndk" \
  "$builder" "$fixture_root/output" >/dev/null 2>&1 || status=$?
test "$status" -eq 73

cat > "$fixture_root/expected-arguments" <<'EOF'
ndk
-t
arm64-v8a
--
build
--release
--config
profile.release.package.covalent-node.opt-level="s"
--config
profile.release.package.covalent-android-jni.opt-level="s"
-p
covalent-android-jni
EOF

if ! cmp -s "$fixture_root/expected-arguments" "$cargo_log"; then
  echo "Android JNI builder did not pass the package-scoped release overrides to cargo-ndk" >&2
  diff -u "$fixture_root/expected-arguments" "$cargo_log" >&2 || true
  exit 1
fi

echo "Android JNI build command contract: ok"
