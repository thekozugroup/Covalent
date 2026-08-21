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
test -x "$package_gate"
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
grep -Fq 'External node connections remain unchanged' "$manager"

echo "Android JNI contract checks passed."
