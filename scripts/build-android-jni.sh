#!/bin/sh
set -eu

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ndk_version=27.1.12297006
ndk_root=${COVALENT_ANDROID_NDK_HOME:?Set COVALENT_ANDROID_NDK_HOME to Android NDK 27.1.12297006}
output_root=${1:-"$repo_root/apps/android/app/build/generated/jniLibs"}

case "$ndk_root" in
  *"/$ndk_version") ;;
  *) echo "COVALENT_ANDROID_NDK_HOME must use Android NDK $ndk_version" >&2; exit 1 ;;
esac
test -d "$ndk_root" || {
  echo "Android NDK $ndk_version is required at $ndk_root" >&2
  exit 1
}
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

mkdir -p "$output_root"
export ANDROID_NDK_HOME="$ndk_root"
export RUSTC="$rustc_bin"
# Hide bundled static-library implementation symbols.  JNI_OnLoad is the only
# intended public entry point; the release audit below enforces that contract.
export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,--exclude-libs,ALL"

cd "$repo_root"
"$cargo_bin" ndk -t arm64-v8a -o "$output_root" build --release -p covalent-android-jni
"$cargo_bin" ndk -t x86_64 -o "$output_root" build --release -p covalent-android-jni

llvm_readobj=$(find "$ndk_root/toolchains/llvm/prebuilt" -type f -path '*/bin/llvm-readobj' -print -quit)
test -x "$llvm_readobj" || {
  echo "The pinned NDK llvm-readobj is required to verify Android JNI libraries" >&2
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
  "$llvm_readobj" --dyn-symbols "$library" > "$symbols_directory/$abi.txt"
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
  test "$(wc -c < "$library")" -le 2097152 || {
    echo "JNI library for $abi exceeds the 2 MiB release budget" >&2
    exit 1
  }
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
