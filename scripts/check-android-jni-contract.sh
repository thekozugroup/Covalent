#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
crate="$repo_root/crates/covalent-android-jni"
native="$repo_root/apps/android/app/src/main/java/life/michaelwong/covalent/node/CovalentNative.kt"
manager="$repo_root/apps/android/app/src/main/java/life/michaelwong/covalent/node/EmbeddedNodeManager.kt"
service="$repo_root/apps/android/app/src/main/java/life/michaelwong/covalent/node/NodeProviderService.kt"
manifest="$repo_root/apps/android/app/src/main/AndroidManifest.xml"
package_gate="$repo_root/scripts/check-android-native-package.sh"
verification_metadata="$repo_root/apps/android/gradle/verification-metadata.xml"
sync_builder="$repo_root/scripts/build-android-sync-engine.sh"
link_provenance="$repo_root/scripts/collect-android-native-link-provenance.py"
go_link_wrapper="$repo_root/scripts/android-go-link-wrapper.sh"
sync_guardian="$repo_root/packaging/sync-engine/engine-guardian.c"
gradle_build="$repo_root/apps/android/app/build.gradle.kts"
packaged_sync="$repo_root/apps/android/app/src/main/java/life/michaelwong/covalent/node/PackagedSyncEngine.kt"

test -f "$crate/Cargo.toml"
test -f "$crate/src/lib.rs"
test -f "$native"
test -f "$manager"
test -f "$service"
test -f "$verification_metadata"
test -x "$sync_builder"
test -x "$link_provenance"
test -x "$go_link_wrapper"
test -f "$sync_guardian"
test -f "$packaged_sync"

# AAPT2 resolves a host-specific executable JAR lazily during resource
# processing. Dependency verification generated on macOS therefore sees only
# the OS X artifact unless Linux is asserted explicitly; that let clean Linux
# CI fail after every other dependency had already verified. Keep both host
# artifacts bound to the exact AGP 9.2.1 component and reviewed checksums.
aapt2_component=$(
  awk '
    /<component group="com.android.tools.build" name="aapt2" version="9.2.1-15009934">/ { inside = 1 }
    inside { print }
    inside && /<\/component>/ { exit }
  ' "$verification_metadata"
)
test -n "$aapt2_component" || {
  echo "AAPT2 9.2.1-15009934 verification component is missing" >&2
  exit 1
}

require_aapt2_checksum() {
  artifact_name=$1
  expected_sha256=$2
  printf '%s\n' "$aapt2_component" | awk \
    -v artifact_name="$artifact_name" \
    -v expected_sha256="$expected_sha256" '
      index($0, "<artifact name=\"" artifact_name "\">") { artifact = 1 }
      artifact && index($0, "<sha256 value=\"" expected_sha256 "\"") { verified = 1 }
      artifact && /<\/artifact>/ { exit }
      END { exit verified ? 0 : 1 }
    ' || {
    echo "$artifact_name must keep its independently reviewed SHA-256 in verification-metadata.xml" >&2
    exit 1
  }
}

require_aapt2_checksum \
  "aapt2-9.2.1-15009934-linux.jar" \
  "755f6727fb3f4cce5e319eac0f3618ed4b36b49a46d4bb2cbb6fa8e9175a54d6"
require_aapt2_checksum \
  "aapt2-9.2.1-15009934-osx.jar" \
  "ece4bbeb8a9b89410943cd83a12446a93741d1a8a249873d830653bd84fb9d44"

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
grep -Fq -- '--pack-dyn-relocs=android' "$repo_root/scripts/build-android-jni.sh"
grep -Fq 'SHT_ANDROID_RELA' "$repo_root/scripts/build-android-jni.sh"
# Android release builds may optimise the node orchestration and JNI adapter
# for size, but a global release override would also slow crypto and streaming
# dependencies. Pin the exact package-scoped Cargo overrides and their position
# after cargo-ndk's `--`, where they are forwarded to `cargo build`.
build_jni="$repo_root/scripts/build-android-jni.sh"
grep -Fq 'node_release_override='\''profile.release.package.covalent-node.opt-level="s"'\''' "$build_jni"
grep -Fq 'jni_release_override='\''profile.release.package.covalent-android-jni.opt-level="s"'\''' "$build_jni"
grep -Fq -- '--config "$node_release_override"' "$build_jni"
grep -Fq -- '--config "$jni_release_override"' "$build_jni"
if grep -Eq 'profile\.release\.opt-level|profile\.release\.package\."\*"\.opt-level' "$build_jni"; then
  echo "Android JNI size optimisation must remain scoped to the node and JNI packages" >&2
  exit 1
fi
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

# The maintained folder engine is built from a git archive of one exact
# official source commit. Its executable guardian is the byte-identical source
# reviewed for the macOS package, and no prebuilt ELF is tracked here.
test "$(shasum -a 256 "$sync_guardian" | awk '{print $1}')" = \
  c50b5cf10a287c4c061b7891978fd2681b5c96910eb2c69c922beae349140579
grep -Fq 'expected_commit=946e2b83a1f6c6ae119427c09e0a5802940b82ff' "$sync_builder"
grep -Fq "expected_go='go version go1.26.7 '" "$sync_builder"
grep -Fq 'expected_ndk=27.1.12297006' "$sync_builder"
grep -Fq 'git -C "$source_dir" archive --format=tar "$expected_commit"' "$sync_builder"
grep -Fq 'GOFLAGS=-mod=readonly' "$sync_builder"
grep -Fq 'GOTMPDIR=' "$sync_builder"
grep -Fq 'TMPDIR=' "$sync_builder"
grep -Fq -- '-goos android' "$sync_builder"
grep -Fq -- '-no-upgrade' "$sync_builder"
grep -Fq -- '-version v2.1.3' "$sync_builder"
grep -Fq 'max-page-size=16384' "$sync_builder"
grep -Fq 'syncthing-link-provenance-$abi.json' "$sync_builder"
grep -Fq 'guardian-link-provenance-$abi.json' "$sync_builder"
grep -Fq 'android-go-link-wrapper.sh' "$sync_builder"
grep -Fq 'COVALENT_LINK_PRIVATE_ROOT=' "$sync_builder"
grep -Fq 'jni-link-provenance-$abi.json' "$repo_root/scripts/build-android-jni.sh"
grep -Fq 'NOTICE.toolchain' "$sync_builder"
grep -Fq -- '--source-archive-suffix .tgz' "$sync_builder"
grep -Fq -- '-Wl,-Map,' "$go_link_wrapper"
grep -Fq -- '-Wl,-Map,' "$repo_root/scripts/build-android-jni.sh"
grep -Fq -- '-###' "$go_link_wrapper"
grep -Fq -- '-###' "$repo_root/scripts/build-android-jni.sh"
grep -Fq 'useLegacyPackaging = true' "$gradle_build"
grep -Fq 'keepDebugSymbols += "**/libcovalent_android_jni.so"' "$gradle_build"
grep -Fq 'keepDebugSymbols += "**/libsyncthing.so"' "$gradle_build"
grep -Fq 'keepDebugSymbols += "**/libengineguardian.so"' "$gradle_build"
grep -Fq 'it.name.startsWith("merge") && it.name.endsWith("Assets")' "$gradle_build"
grep -Fq 'mustRunAfter(buildAndroidSyncEngine)' "$gradle_build"
grep -Fq 'COVALENT_SYNC_ENGINE_PACKAGED' "$gradle_build"
grep -Fq 'PackagedSyncEnginePackage.Invalid' "$packaged_sync"
grep -Fq 'folder_sync_package_invalid' "$crate/src/lib.rs"
grep -Fq 'folder_sync_access_unavailable' "$crate/src/lib.rs"
grep -Fq 'backup_provider_enabled' "$crate/src/lib.rs"
grep -Fq 'configuration.local_provider_enabled = backup_provider_enabled' "$crate/src/lib.rs"
grep -Fq 'folderSyncAccessUnavailable' "$native"
grep -Fq 'backupProviderEnabled' "$native"
grep -Fq 'folderSyncAccessUnavailable()' "$manager"
if git -C "$repo_root" ls-files '*.so' | \
  grep -E '(^|/)(libsyncthing|libengineguardian)\.so$' >/dev/null; then
  echo "Prebuilt folder-engine executables must not be committed to the product source" >&2
  exit 1
fi
"$repo_root/scripts/test-android-native-package.sh"

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
