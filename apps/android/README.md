# Android app

End users should follow the
[verified APK and onboarding guide](../../docs/platform/android.md). This file
is for source development and validation.

## Supported target

Android is Tier 1. Release evidence uses Android 17 / API 37. The native
Kotlin/Jetpack Compose app uses AGP 9.2.1, built-in Kotlin, Material 3, and Rust
JNI libraries for `arm64-v8a` and `x86_64`.

Required local tools:

- JDK 17 through 25 (CI uses 17);
- Android SDK Platform 37.0 and Build Tools 37.0.0;
- Android Platform Tools;
- NDK 27.1.12297006;
- the `rustup` toolchain pinned by `rust-toolchain.toml`;
- Rust targets `aarch64-linux-android` and `x86_64-linux-android`; and
- `cargo-ndk` 4.1.2.

Set these paths before Gradle work:

```sh
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export ANDROID_SDK_ROOT="$ANDROID_HOME"
export COVALENT_ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.1.12297006"
```

Then run the read-only prerequisite check from repository root:

```sh
./scripts/setup-doctor.sh android
```

## Build the personal APK

Use the guarded end-to-end builder from repository root:

```sh
./scripts/build-personal-android-apk.sh
```

It runs the setup doctor, builds current Rust JNI libraries for both supported
ABIs, assembles the debug-signed APK, verifies signer/package/version/native
content, and copies the APK plus checksum to `artifacts/install/` without
overwriting an existing file. It never installs unless given `--install` or
`--update` with one exact device serial. See the
[personal Android guide](../../docs/platform/android.md) for those commands and
the debug-key update boundary.

## Build a complete debug APK manually

From repository root:

```sh
./apps/android/gradlew -p apps/android --no-daemon \
  --dependency-verification=strict \
  -PcovalentBuildNative=true assembleDebug
```

Output:

```text
apps/android/app/build/outputs/apk/debug/app-debug.apk
```

`-PcovalentBuildNative=true` is mandatory for a fresh debug APK containing the
Rust node runtime. Gradle signs this APK with the local debug key, making it
installable for personal testing. It is not a permanent update identity.

Quick artifact checks:

```sh
apk=apps/android/app/build/outputs/apk/debug/app-debug.apk
build_tools="$ANDROID_HOME/build-tools/37.0.0"
"$build_tools/apksigner" verify --verbose --print-certs "$apk"
test "$("$build_tools/aapt2" dump packagename "$apk")" = life.michaelwong.covalent
```

Do not install or distribute `app-release-unsigned.apk`. Future production
signing needs one durable signer. Android cannot update a debug-signed install
with that signer, and debug APKs built under different debug keys cannot update
each other. Preserve tested server backups before any required uninstall.

## Run checks

Full host gate, from repository root:

```sh
./scripts/check-android.sh
```

Headed device gate requires exact AVD `Covalent_API_37` on
`emulator-5570`. Start that AVD first, keep its window visible, then run:

```sh
ANDROID_SERIAL=emulator-5570 ./scripts/android-api37-device-test.sh
```

The device gate builds or verifies current artifacts, runs every connected
Compose test, installs and launches through `mobilecli`, captures first-launch
evidence, and exercises real authenticated TLS against the packaged node. Never
run this gate against another emulator.

Generate dependency and license evidence without production signing material:

```sh
./apps/android/gradlew -p apps/android --no-daemon :app:generateAndroidSbom
```

## Runtime design

The client uses the versioned local-node API in `docs/api/openapi.yaml`; product
actions never route to a mock. It stores bearer credentials, exact CA or
certificate-pin trust, queued request payloads, and transfer journals with
Android-protected storage. Remote bearer authentication requires HTTPS with
normal hostname verification. Plain HTTP is accepted only for a loopback node;
there is no trust-all mode.

Folder access uses persisted Storage Access Framework tree grants. Backup
streams a validated ZIP through `ParcelFileDescriptor`; the node never receives
a `content://` URI. Restore inventories an authorized writable destination,
requires a durable signed no-write plan, and applies Stop, Skip, or Keep both.
Replace is intentionally unavailable on general Android document providers.

WorkManager jobs use stable IDs, bounded scheduling, network constraints,
foreground notification, durable progress/error state, pause/resume/cancel,
and encrypted pending payloads. Archive responses are acknowledged only after
successful SAF processing.

Pairing treats LAN and Tailnet discovery as untrusted hints. Both devices must
compare the same authentication code, approve exact roles, retain both
signatures, and pin the signed transport certificate. Provider selection stays
explicit for each backup; no provider means the connected server keeps the only
copy.
