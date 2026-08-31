# Set up Covalent on Android

Current personal-use path: build the debug APK from this repository. Gradle
signs that APK with your local debug key, so Android can install it. Never
install `app-release-unsigned.apk`; an unsigned release APK is not an
alternative.

Android 17 / API 37 is the supported and release-tested target. The manifest
allows API 26 and later, but older Android versions are not Tier 1 release
evidence.

## Before you start

You need:

- a trusted Mac or Linux computer with this repository;
- JDK 17 through 25 (CI uses 17);
- Android SDK Platform 37.0, Build Tools 37.0.0, Platform Tools, and NDK
  27.1.12297006;
- `rustup` with the repository's pinned Rust toolchain;
- `cargo-ndk` 4.1.2;
- an Android device with USB debugging enabled; and
- a claimed Docker or Unraid server, such as Atlas.

## Build the installable personal APK

From the repository root, run one command:

```sh
./scripts/build-personal-android-apk.sh
```

It checks prerequisites, builds both native ABIs, verifies the debug signer,
package, version, alignment, and native libraries, then writes an APK and its
SHA-256 file under the ignored `artifacts/install/` directory. It does not
connect to or change any Android device. Gradle's intermediate file is
`apps/android/app/build/outputs/apk/debug/app-debug.apk`; install the verified
copy under `artifacts/install/`.

The output name includes the app version and the first 16 characters of its
SHA-256, for example:

```text
artifacts/install/Covalent-v0.2.0-android-personal-debug-0123456789abcdef.apk
artifacts/install/Covalent-v0.2.0-android-personal-debug-0123456789abcdef.apk.sha256
```

Existing artifacts are never overwritten. An identical verified build is
reused; a conflicting file stops the command.

If the prerequisite check reports missing tools, install the exact Android and
Rust inputs below. Set `ANDROID_HOME` to your SDK directory first. On macOS,
Android Studio normally uses `$HOME/Library/Android/sdk`.

```sh
export ANDROID_HOME="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
export ANDROID_SDK_ROOT="$ANDROID_HOME"
export COVALENT_ANDROID_NDK_HOME="$ANDROID_HOME/ndk/27.1.12297006"

sdkmanager \
  "platform-tools" \
  "platforms;android-37.0" \
  "build-tools;37.0.0" \
  "ndk;27.1.12297006"
rustup target add aarch64-linux-android x86_64-linux-android
cargo install --locked cargo-ndk --version 4.1.2
```

If `sdkmanager` is not on `PATH`, run the copy under Android SDK Command-line
Tools, commonly `$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager`.

The command uses `-PcovalentBuildNative=true`; a plain Gradle debug build can
reuse or omit native Rust output. The printed APK SHA-256 identifies the exact
file. The printed certificate SHA-256 identifies this computer's debug signer.
Neither turns a personal debug key into a publisher identity.

## Install or update

Connect one unlocked Android device, approve its USB-debugging prompt, then
copy its exact serial from:

```sh
adb devices
```

Choose one command and replace `DEVICE_SERIAL` with that exact value:

```sh
# New install:
./scripts/build-personal-android-apk.sh --install DEVICE_SERIAL

# Update made with the same debug key:
./scripts/build-personal-android-apk.sh --update DEVICE_SERIAL
```

Use `--install` for a new install. Use `--update` only for an existing Covalent
app signed by the same debug key. The command never picks a device implicitly
and never uninstalls an app. If the serial begins with `emulator-`, only exact
AVD `Covalent_API_37` on `emulator-5570` is accepted.

Confirm the installed package, then launch it:

```sh
device_serial=DEVICE_SERIAL
adb -s "$device_serial" shell pm path life.michaelwong.covalent
adb -s "$device_serial" shell am start -n life.michaelwong.covalent/.MainActivity
```

Stop on `INSTALL_FAILED_UPDATE_INCOMPATIBLE`: the installed app and new APK use
different signers. Building on another computer normally creates another debug
key. Uninstalling fixes the signer conflict but deletes Covalent's app-local
settings, protected connection, pending work, and Android folder grants. Do
not uninstall until completed backups have passed a restore check and the
claim files remain available.

A future production-signed APK also cannot update this debug-signed install.
Moving to that permanent signer will require one deliberate uninstall and
re-enrollment. After that move, every update must keep the same production key.

## Claim the backup server

Claim each new server once from a trusted Mac or Linux computer using the
verified `covalent` CLI. Follow the [CLI install guide](../release/cli-install.md)
and [Atlas claim steps](atlas-tailscale.md#3-claim-over-https-then-enroll-the-exact-ca).

Successful claim output is an owner-only directory containing:

- `root.crt` — exact private CA certificate Android enrolls; and
- `local-api-token` — secret bearer token Android protects.

Transfer both files to the phone over a trusted direct channel. The one-time
setup code is only for `covalent claim`: never copy, paste, or type it into the
app. Never copy the server's `/config` directory or CA signing key to Android.

Keep the original owner-only claim directory on the trusted computer. After
Android reports **Backup server ready**, delete temporary phone copies of the
token and certificate; Covalent retains its protected enrollment.

## Choose LAN or Tailnet

Tailscale is optional. Use one route:

- **LAN:** keep the phone and server on the same trusted network. Use the exact
  HTTPS hostname configured on the server, make sure it resolves on the phone,
  allow TCP 8443, and grant Android's local-network prompt.
- **Tailnet:** install Tailscale on the phone, sign in to the intended Tailnet,
  and use the server's exact MagicDNS name. Tailnet policy must allow TCP 8443.

The hostname in the URL must match the server certificate. Do not replace it
with a raw IP unless that IP is in the certificate. Do not use cleartext HTTP
or a trust-all tool.

UDP 8787 is needed only when this phone pairs with another Covalent device or
uses one as an extra-copy provider. Allow it on the chosen LAN or Tailnet path.
TCP 8443 remains the HTTPS console/API path.

## Connect in Covalent

On **Connect your backup server**:

1. Enter a device name.
2. Enter the exact HTTPS address claimed for the server, including port 8443.
3. Choose **Choose token file** and select `local-api-token`. If the platform
   picker cannot access the transfer location, paste the exact contents into
   **Server access token** instead.
4. Choose **Choose security certificate** and select `root.crt`.
5. Leave **Certificate fingerprint** empty; use one private trust method, not
   both.
6. Choose **Connect** and grant local-network access if Android asks.

Continue only when Home says **Backup server ready**. Covalent verifies the
CA, hostname, token, and live server status before enabling actions.

## Complete the first recovery checkpoint

Use a small, expendable folder first:

1. In Android's Files app, create a test folder and one file with recognizable
   contents.
2. In Covalent, choose **Backup**, then **Choose source folder** and select only
   that test folder.
3. Name the backup. Leave extra devices unselected unless you already paired
   one; Atlas still keeps the primary encrypted backup.
4. Review the source and copy count, then choose **Queue backup**.
5. Return to Home. Wait for the transfer to say **Completed** and for the
   remembered backup to show a snapshot.
6. On that backup card, choose **Verify**. Continue only when the app says
   **Verified: everything checked is intact.**
7. Choose **Restore**. Select the remembered backup and **Stop without
   writing**, then choose a different empty writable folder.
8. Choose **Preview restore**, inspect every signed path, then **Queue restore**.
9. Wait for Home to report **Restore complete**. Open the restored file and
   compare it with the original.

Checkpoint passes only after backup, verification, and separate-folder restore
all succeed. Covalent uses Android's folder picker for backup sources and
writable restore destinations; never grant all-files access.

For a stronger source-loss drill, move the expendable source file elsewhere
after verification, restore it into another empty folder, compare it, then put
the source back. Do not test with irreplaceable data.

## Troubleshooting

- **`sdkmanager` missing:** install Android SDK Command-line Tools in Android
  Studio and add its `latest/bin` directory to `PATH`.
- **Native build asks for an NDK:** confirm
  `COVALENT_ANDROID_NDK_HOME` ends in `27.1.12297006` and both Rust Android
  targets are installed.
- **`adb` shows unauthorized:** unlock the phone, accept its debugging prompt,
  then run `adb devices` again.
- **Server cannot be verified:** use the exact claimed HTTPS hostname, correct
  `root.crt`, and exact `local-api-token`. Never bypass TLS.
- **LAN works on a computer but not Android:** grant local-network access and
  confirm Wi-Fi client isolation is off.
- **Tailnet fails:** confirm the phone is online in the intended Tailnet and its
  policy allows TCP 8443. Add UDP 8787 only for device pairing/replicas.
- **Folder access was revoked:** choose the folder again. Reinstalling the app
  always removes its saved folder grants.

More recovery decisions: [setup troubleshooting](../troubleshooting.md).
