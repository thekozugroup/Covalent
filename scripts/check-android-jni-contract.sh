#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
crate="$repo_root/crates/covalent-android-jni"
native="$repo_root/apps/android/app/src/main/java/life/michaelwong/covalent/node/CovalentNative.kt"
manager="$repo_root/apps/android/app/src/main/java/life/michaelwong/covalent/node/EmbeddedNodeManager.kt"
service="$repo_root/apps/android/app/src/main/java/life/michaelwong/covalent/node/NodeProviderService.kt"
manifest="$repo_root/apps/android/app/src/main/AndroidManifest.xml"
package_gate="$repo_root/scripts/check-android-native-package.sh"

test -f "$crate/Cargo.toml"
test -f "$crate/src/lib.rs"
test -f "$native"
test -f "$manager"
test -f "$service"
# This is only a presence check: the package gate needs a built APK or AAB and a
# zipalign binary, so it cannot run here. It is invoked for real on the signed
# artefacts in .github/workflows/android-release.yml ("Verify the signed native
# packages"). Assert that caller still exists, so deleting it cannot quietly
# return this script to being the gate's only mention.
test -x "$package_gate"
grep -Fq 'scripts/check-android-native-package.sh' \
  "$repo_root/.github/workflows/android-release.yml" || {
  echo "check-android-native-package.sh has no release-workflow caller; it would run on nothing." >&2
  exit 1
}
# The JNI library is linked from a staticlib by scripts/build-android-jni.sh so
# that exports.map governs the export surface; a cdylib cannot, because rustc
# globals every `#[no_mangle]` symbol in the crate graph in its own version
# script. Assert both halves of that arrangement stay in place.
grep -Fq 'crate-type = ["staticlib"]' "$crate/Cargo.toml"
test -f "$crate/exports.map"
grep -Fq 'JNI_OnLoad;' "$crate/exports.map"
grep -Fq 'local:' "$crate/exports.map"
grep -Fq -e '--version-script' "$repo_root/scripts/build-android-jni.sh"
grep -Fq 'JNI_OnLoad' "$crate/src/lib.rs"
grep -Fq 'register_native_methods' "$crate/src/lib.rs"
grep -Fq 'MAX_LIVE_NODES' "$crate/src/lib.rs"
grep -Fq 'secure_key_protector_required' "$crate/src/lib.rs"
grep -Fq 'noBackupFilesDir' "$manager"
grep -Fq 'NodeProviderService' "$service"
grep -Fq 'FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE' "$service"
grep -Fq 'FOREGROUND_SERVICE_CONNECTED_DEVICE' "$manifest"
grep -Fq 'foregroundServiceType="connectedDevice"' "$manifest"
grep -Fq 'nativeStart' "$native"

# The embedded on-device provider is an explicit opt-in that must never disturb
# a separately configured external node. That contract used to be asserted by
# grepping "$manager" for the sentence "External node connections remain
# unchanged" - a string shown to users. Pinning UI copy in a shell script made
# any wording improvement look like a security regression, and it proved
# nothing: the sentence could stay word-perfect while the code beneath it
# started clobbering external state. Assert the behaviour instead.

# 1. Disabling the local provider must hand control back to external mode, so
#    the app is never left pointed at a local node it has just stopped, and must
#    stop only the embedded provider's own service.
disable_body=$(
  awk '
    /^    fun disable\(\)/ { inside = 1 }
    inside { print }
    inside && /^    \}/ { exit }
  ' "$manager"
)
test -n "$disable_body" || {
  echo "EmbeddedNodeManager.disable() was not found; the external-node contract is unverifiable" >&2
  exit 1
}
printf '%s\n' "$disable_body" | grep -Fq 'putString(KEY_ACTIVE_MODE, NodeMode.EXTERNAL.wireValue)' || {
  echo "disable() must restore external mode so stopping the local provider cannot strand the app" >&2
  exit 1
}
printf '%s\n' "$disable_body" | grep -Fq 'NodeProviderService.ACTION_STOP' || {
  echo "disable() must stop only the embedded provider service" >&2
  exit 1
}

# 2. External mode is both the default and the fallback for an unknown persisted
#    value, so no parse failure can silently promote the local node.
grep -Fq 'preferences.getString(KEY_ACTIVE_MODE, NodeMode.EXTERNAL.wireValue)' "$manager" || {
  echo "activeMode() must default to external mode" >&2
  exit 1
}
grep -Eq '\?:[[:space:]]*EXTERNAL' "$manager" || {
  echo "NodeMode.fromWire must fall back to EXTERNAL for unknown wire values" >&2
  exit 1
}

# 3. Local credentials live in their own store and are handed to the client
#    selector only while local mode is active, so external credentials are never
#    read, written, or substituted by this path.
grep -Fq 'activeMode() == NodeMode.LOCAL' "$manager" || {
  echo "local credentials must only be returned while local mode is active" >&2
  exit 1
}
grep -Fq 'covalent_embedded_node_credentials' "$manager" || {
  echo "local node credentials must use their own separate store" >&2
  exit 1
}

echo "Android JNI contract checks passed."
