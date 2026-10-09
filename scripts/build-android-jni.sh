#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
# Single source of truth for the per-ABI size floor and ceiling, shared with
# scripts/check-android-native-package.sh so the two gates cannot drift.
. "$repo_root/scripts/android-native-budgets.sh"
ndk_version=27.1.12297006
ndk_root=${COVALENT_ANDROID_NDK_HOME:?Set COVALENT_ANDROID_NDK_HOME to Android NDK 27.1.12297006}
output_root=${1:-"$repo_root/apps/android/app/build/generated/jniLibs"}
link_provenance_tool="$repo_root/scripts/collect-android-native-link-provenance.py"

case "$ndk_root" in
  *"/$ndk_version") ;;
  *) echo "COVALENT_ANDROID_NDK_HOME must use Android NDK $ndk_version" >&2; exit 1 ;;
esac
test -d "$ndk_root" || {
  echo "Android NDK $ndk_version is required at $ndk_root" >&2
  exit 1
}
ndk_root=$(CDPATH='' cd -- "$ndk_root" && pwd -P)
test -f "$ndk_root/source.properties" && test ! -L "$ndk_root/source.properties" || {
  echo "Android NDK source.properties must be a regular non-symbolic-link file" >&2
  exit 1
}
actual_ndk=$(awk -F= '
  $1 ~ /^Pkg\.Revision[[:space:]]*$/ {
    value = $2
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
    print value
    exit
  }
' "$ndk_root/source.properties")
test "$actual_ndk" = "$ndk_version" || {
  echo "Expected Android NDK $ndk_version" >&2
  exit 1
}
for notice_name in NOTICE NOTICE.toolchain; do
  notice_path="$ndk_root/$notice_name"
  test -f "$notice_path" && test ! -L "$notice_path" || {
    echo "NDK $notice_name must be a regular non-symbolic-link file" >&2
    exit 1
  }
  notice_size=$(wc -c < "$notice_path" | tr -d '[:space:]')
  test "$notice_size" -gt 0 && test "$notice_size" -le 2097152 || {
    echo "NDK $notice_name is empty or exceeds 2 MiB" >&2
    exit 1
  }
done
command -v rustup >/dev/null 2>&1 || {
  echo "cargo-ndk 4.1.2 is required: cargo install --locked cargo-ndk --version 4.1.2" >&2
  exit 1
}
cargo_bin=$(rustup which cargo)
rustc_bin=$(rustup which rustc)
test -x "$cargo_bin" || {
  echo "rustup-managed Cargo is required for Android cross-compilation" >&2
  exit 1
}
test -x "$rustc_bin" || {
  echo "rustup-managed rustc is required for Android cross-compilation" >&2
  exit 1
}
test "$("$cargo_bin" ndk --version | awk '{print $2}')" = "4.1.2" || {
  echo "cargo-ndk 4.1.2 is required" >&2
  exit 1
}

for target in aarch64-linux-android x86_64-linux-android; do
  rustup target list --installed | grep -Fx "$target" >/dev/null || {
    echo "Rust target $target is required" >&2
    exit 1
  }
done

test ! -e "$output_root" && test ! -L "$output_root" || {
  echo "Generated Android JNI output must be a new path" >&2
  exit 1
}
test -d "$(dirname -- "$output_root")" || {
  echo "Generated Android JNI output parent is missing" >&2
  exit 1
}
mkdir "$output_root"
output_root=$(CDPATH='' cd -- "$output_root" && pwd -P)
export ANDROID_NDK_HOME="$ndk_root"
export RUSTC="$rustc_bin"

cd "$repo_root"

# The crate is built as a staticlib and linked into the shared object here,
# by us, instead of letting rustc emit a cdylib.
#
# JNI_OnLoad is the only intended public entry point, and the audit below
# enforces that.  rustc cannot honour that contract for a cdylib: it emits its
# own `--version-script` listing every `#[no_mangle]` symbol in the *entire
# crate graph* as global.  blake3 1.8.6 defines
# `blake3_compress_in_place_portable` as a `#[no_mangle]` *Rust* function in
# `src/ffi_neon.rs` (it backs the 1x compression that `c/blake3_neon.c` needs),
# and that module is compiled only under `cfg(blake3_neon)` — which blake3's
# build.rs sets on aarch64/armv7 and never on x86_64.  That is exactly why the
# arm64 library exported a second symbol while x86_64 stayed clean.
#
# Because rustc globals it in its own version script, nothing downstream can
# demote it: `--exclude-libs` only rewrites symbols coming from archive
# members (rustc passes crate objects to the linker directly), a second
# `--version-script` loses to rustc's exact-match `global:` entry, and objcopy
# will not rewrite `.dynsym` of an already-linked shared object.
#
# Linking the staticlib ourselves puts exports.map in charge, so the contract
# holds on both ABIs and blake3 keeps its NEON backend on arm64.
android_api=26  # must match minSdk in apps/android/app/build.gradle.kts
version_script="$repo_root/crates/covalent-android-jni/exports.map"
target_dir=${CARGO_TARGET_DIR:-"$repo_root/target"}
# Keep crypto, storage, protocol, and networking dependencies at the workspace's
# release opt-level. Only the node's Android-facing orchestration and the thin
# JNI adapter trade a little instruction-level optimisation for code size. The
# overrides cannot change the inherited fat LTO, single codegen unit, stripping,
# or unwind panic strategy; JNI entry points therefore retain catch_unwind.
node_release_override='profile.release.package.covalent-node.opt-level="s"'
jni_release_override='profile.release.package.covalent-android-jni.opt-level="s"'
test -f "$version_script" || {
  echo "Missing JNI export version script at $version_script" >&2
  exit 1
}
provenance_directory="$output_root/provenance"
mkdir "$provenance_directory"
llvm_bin=""

for abi in arm64-v8a x86_64; do
  case "$abi" in
    arm64-v8a) triple=aarch64-linux-android ;;
    x86_64) triple=x86_64-linux-android ;;
    *) echo "Unsupported ABI $abi" >&2; exit 1 ;;
  esac

  "$cargo_bin" ndk -t "$abi" -- build --release \
    --config "$node_release_override" \
    --config "$jni_release_override" \
    -p covalent-android-jni

  archive="$target_dir/$triple/release/libcovalent_android_jni.a"
  test -f "$archive" || {
    echo "Missing JNI static library for $abi at $archive" >&2
    exit 1
  }

  clang_bin=$(find "$ndk_root/toolchains/llvm/prebuilt" -type f \
    -name "${triple}${android_api}-clang" -print -quit)
  test -x "$clang_bin" || {
    echo "The pinned NDK clang driver for $triple$android_api is required" >&2
    exit 1
  }
  candidate_llvm_bin=$(dirname -- "$clang_bin")
  test -z "$llvm_bin" || test "$candidate_llvm_bin" = "$llvm_bin" || {
    echo "Both Android ABI drivers must use one pinned NDK host toolchain" >&2
    exit 1
  }
  llvm_bin=$candidate_llvm_bin
  link_map="$provenance_directory/jni-link-$abi.map"
  driver_trace="$provenance_directory/jni-driver-$abi.txt"

  # --no-undefined keeps a missing runtime dependency a link error rather than
  # a load-time crash; --strip-all only drops .symtab, leaving .dynsym (and so
  # the export contract) intact. Android's packed relocation format is
  # supported from API 23; minSdk is API 26, so this reduces load metadata on
  # every supported device without relying on unsafe identical-code folding.
  mkdir -p "$output_root/$abi"
  "$clang_bin" -### -shared -o "$output_root/$abi/libcovalent_android_jni.so" \
    -Wl,--version-script="$version_script" \
    -Wl,--undefined=JNI_OnLoad \
    -Wl,--gc-sections \
    -Wl,--pack-dyn-relocs=android \
    -Wl,--hash-style=both \
    -Wl,--no-undefined \
    -Wl,-z,max-page-size=16384 \
    -Wl,-z,relro \
    -Wl,-z,now \
    -Wl,-z,noexecstack \
    -Wl,--strip-all \
    -Wl,-Map,"$link_map" \
    "$archive" \
    -llog -ldl -lm -lc >/dev/null 2> "$driver_trace"
  test -s "$driver_trace" && test "$(wc -c < "$driver_trace" | tr -d '[:space:]')" -le 4194304 || {
    echo "JNI Clang driver trace for $abi is empty or exceeds 4 MiB" >&2
    exit 1
  }
  "$clang_bin" -shared -o "$output_root/$abi/libcovalent_android_jni.so" \
    -Wl,--version-script="$version_script" \
    -Wl,--undefined=JNI_OnLoad \
    -Wl,--gc-sections \
    -Wl,--pack-dyn-relocs=android \
    -Wl,--hash-style=both \
    -Wl,--no-undefined \
    -Wl,-z,max-page-size=16384 \
    -Wl,-z,relro \
    -Wl,-z,now \
    -Wl,-z,noexecstack \
    -Wl,--strip-all \
    -Wl,-Map,"$link_map" \
    "$archive" \
    -llog -ldl -lm -lc
  test -s "$link_map" && test "$(wc -c < "$link_map" | tr -d '[:space:]')" -le 134217728 || {
    echo "JNI final link map for $abi is empty or exceeds 128 MiB" >&2
    exit 1
  }
done

# Use the same pinned host toolchain directory that supplied both ABI drivers.
# NDK r27b installs llvm-readelf as a symlink to llvm-readobj, so a `find
# -type f` lookup incorrectly rejects the official tool after both links have
# already completed. `test -x` below still rejects a missing or broken link.
llvm_readobj="$llvm_bin/llvm-readobj"
llvm_readelf="$llvm_bin/llvm-readelf"
test -x "$llvm_readobj" || {
  echo "The pinned NDK llvm-readobj is required to verify Android JNI libraries" >&2
  exit 1
}
test -x "$llvm_readelf" || {
  echo "The pinned NDK llvm-readelf is required to record Android JNI dependencies" >&2
  exit 1
}

symbols_directory="$output_root/symbols"
mkdir -p "$symbols_directory"

for abi in arm64-v8a x86_64; do
  library="$output_root/$abi/libcovalent_android_jni.so"
  test -f "$library" || {
    echo "Missing JNI library for $abi" >&2
    exit 1
  }
  "$llvm_readobj" --file-headers "$library" | grep -Fq 'Type: SharedObject' || {
    echo "JNI library for $abi is not a shared ELF" >&2
    exit 1
  }
  "$llvm_readobj" --program-headers "$library" | awk '
    /Type: PT_LOAD/ { in_load = 1; loads += 1; next }
    in_load && /Alignment:/ {
      if ($2 + 0 < 16384) bad = 1
      in_load = 0
    }
    END { exit (loads > 0 && !bad) ? 0 : 1 }
  ' || {
    echo "Every PT_LOAD segment for $abi must have at least 16 KiB alignment" >&2
    exit 1
  }
  "$llvm_readobj" --dynamic-table "$library" | grep -Eq 'TEXTREL|DT_TEXTREL' && {
    echo "JNI library for $abi contains forbidden text relocations" >&2
    exit 1
  }
  # `--pack-dyn-relocs=android` is a minSdk-26 compatibility guarantee, not an
  # optional size hint. Verify both the dynamic tags and the section type so a
  # linker/toolchain change cannot silently expand relocations again.
  "$llvm_readobj" --dynamic-table "$library" | grep -Fq 'ANDROID_RELA' || {
    echo "JNI library for $abi is missing Android packed relocation metadata" >&2
    exit 1
  }
  "$llvm_readobj" --dynamic-table "$library" | grep -Fq 'ANDROID_RELASZ' || {
    echo "JNI library for $abi is missing the Android packed relocation size" >&2
    exit 1
  }
  "$llvm_readobj" --sections "$library" | grep -Fq 'SHT_ANDROID_RELA' || {
    echo "JNI library for $abi is missing an Android packed relocation section" >&2
    exit 1
  }
  "$llvm_readobj" --dyn-symbols "$library" > "$symbols_directory/$abi.txt"
  dynamic_report="$provenance_directory/jni-dynamic-$abi.txt"
  "$llvm_readelf" -dW "$library" > "$dynamic_report"
  test "$(wc -c < "$dynamic_report" | tr -d '[:space:]')" -le 1048576 || {
    echo "JNI dynamic report for $abi exceeds 1 MiB" >&2
    exit 1
  }
  python3 "$link_provenance_tool" \
    --component jni \
    --abi "$abi" \
    --ndk-root "$ndk_root" \
    --expected-revision "$ndk_version" \
    --link-map "$provenance_directory/jni-link-$abi.map" \
    --driver-trace "$provenance_directory/jni-driver-$abi.txt" \
    --dynamic-report "$dynamic_report" \
    --binary "$library" \
    --output "$provenance_directory/jni-link-provenance-$abi.json"
  # Audit only symbols this library *defines*. The previous `sed` matched every
  # `Name:` line in the report, so it also flagged all ~68 imported libc symbols
  # (`read@LIBC`, `malloc@LIBC`, ...) plus the report's own `LoadName: <Not
  # found>` header — the gate could never pass and so never reported the one
  # symbol that actually mattered. Parse per-symbol blocks and skip undefined
  # (imported) entries so this measures the export surface it claims to.
  unexpected_symbols=$(awk '
    /^  Symbol \{/ { name = ""; section = ""; in_block = 1; next }
    in_block && /Name:/ {
      line = $0
      sub(/.*Name: */, "", line)
      sub(/ *\(.*/, "", line)
      name = line
      next
    }
    in_block && /Section:/ { section = $2; next }
    in_block && /^  \}/ {
      if (name != "" && section != "Undefined") print name
      in_block = 0
      next
    }
  ' "$symbols_directory/$abi.txt" | sort -u | grep -vx 'JNI_OnLoad' || true)
  test -z "$unexpected_symbols" || {
    echo "JNI library for $abi exports unexpected dynamic symbols: $unexpected_symbols" >&2
    exit 1
  }
  library_bytes=$(wc -c < "$library")
  test "$library_bytes" -le "$COVALENT_JNI_MAX_BYTES" || {
    echo "JNI library for $abi is $library_bytes bytes, over the ${COVALENT_JNI_MAX_BYTES}-byte release budget" >&2
    exit 1
  }
  # A maximum alone cannot catch the failure that produced the old 2 MiB number.
  # See the derivation above: an empty library passes any ceiling. The floor is
  # what turns "the runtime got dead-stripped again" into a build failure.
  test "$library_bytes" -ge "$COVALENT_JNI_MIN_BYTES" || {
    echo "JNI library for $abi is only $library_bytes bytes, under the ${COVALENT_JNI_MIN_BYTES}-byte floor." >&2
    echo "A library this small cannot contain the node runtime; it has almost certainly been dead-stripped." >&2
    exit 1
  }
  echo "  $abi JNI library: $library_bytes bytes (floor $COVALENT_JNI_MIN_BYTES, budget $COVALENT_JNI_MAX_BYTES)"
done

metadata_file="$output_root/covalent-android-native-metadata.json"
"$cargo_bin" metadata --locked --format-version 1 > "$metadata_file"
jq '{
  bomFormat: "CycloneDX",
  specVersion: "1.5",
  serialNumber: "urn:uuid:00000000-0000-4000-8000-000000000000",
  version: 1,
  components: [.packages[] | {
    type: "library", name, version, license,
    purl: (if .source == null then null else "pkg:cargo/" + .name + "@" + .version end)
  }]
}' "$metadata_file" > "$output_root/covalent-android-native-sbom.json"
jq '[.packages[] | {name, version, license, license_file, source}]' "$metadata_file" \
  > "$output_root/covalent-android-native-licenses.json"
rm "$metadata_file"
shasum -a 256 "$output_root"/arm64-v8a/libcovalent_android_jni.so \
  "$output_root"/x86_64/libcovalent_android_jni.so \
  "$output_root"/covalent-android-native-sbom.json \
  "$output_root"/covalent-android-native-licenses.json \
  > "$output_root/covalent-android-native-checksums.sha256"
tar -czf "$output_root/covalent-android-native-symbols.tar.gz" -C "$symbols_directory" .
shasum -a 256 \
  "$provenance_directory"/jni-driver-*.txt \
  "$provenance_directory"/jni-dynamic-*.txt \
  "$provenance_directory"/jni-link-*.map \
  "$provenance_directory"/jni-link-provenance-*.json \
  > "$output_root/covalent-android-native-provenance.sha256"
