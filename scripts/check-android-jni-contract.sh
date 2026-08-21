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
grep -Fq 'crate-type = ["cdylib"]' "$crate/Cargo.toml"
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
