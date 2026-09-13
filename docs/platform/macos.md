# Set up Covalent on macOS

Covalent supports macOS 15 or later on Apple Silicon. The app starts its own
private Covalent node automatically. Use the build and installation steps below,
then [create a one-way link](../getting-started-links.md). Pair a destination
with the Mac's local node; keep the app connected to its own node.

Atlas is offline. Docker is the accepted server target; current native release
acceptance remains open.

## Before you start

You need:

- an Apple Silicon Mac (`arm64`), not an Intel Mac;
- macOS 15 or later;
- another device running the current Covalent build; and
- a small folder for your first transfer.

Check the Mac architecture:

```sh
test "$(uname -m)" = arm64
```

No output means the check passed.

## 1. Install the personal-use app

The personal-use app is ad-hoc signed and is not notarized, so macOS may show an
unidentified-developer warning. Verify it before making any one-time macOS
exception. No notarization action is part of setup. Never disable Gatekeeper
globally.

`unsigned` in the archive name means no Apple distribution identity was used;
the app inside still has the ad-hoc code signature verified below.

### Current path: build the arm64 app from source

Install Xcode 26, open it once, accept its license, and install `rustup`. Then
run one command from the repository root:

```sh
./scripts/build-personal-macos-app.sh
```

The builder checks the Mac and pinned toolchain, installs checksum-pinned
XcodeGen into a private temporary directory, builds the locked source, ad-hoc
signs the app, verifies both arm64 executables, creates the ZIP and checksum,
then extracts and verifies the ZIP again. Finished install files go only to the
ignored `artifacts/install` directory. XcodeGen also creates or refreshes the
ignored generated project at `apps/apple/Covalent.xcodeproj`; tracked source is
not changed. The builder refuses to overwrite an existing artifact and never
installs or replaces an app.

The output names use the current repository version. Verify the finished ZIP
once more before installing it:

```sh
version="$(./scripts/release-version.sh print)"
archive="Covalent-v${version}-macOS-arm64-personal.zip"
(
  cd artifacts/install
  shasum -a 256 -c "${archive}.sha256"
)
open artifacts/install
```

Continue only when the checksum prints `OK`. In Finder, double-click the ZIP,
then drag `Covalent.app` into Applications. If either output already exists,
move that exact pair elsewhere or remove it only after deciding it is no longer
needed; the builder will not replace it. Developer build and test details live
in the [Apple client README](../../apps/apple/README.md).

### After publication: download the verified release build

The v0.2.0 macOS assets are not published yet. Do not use this download path
until the official release page contains both files listed below. The
historical v0.1.0 archive is not a current Covalent setup.

From the official
[GitHub Releases page](https://github.com/thekozugroup/Covalent/releases),
download both files for the same version:

- `Covalent-v0.2.0-macOS-arm64-unsigned.zip`
- `Covalent-v0.2.0-macOS-arm64-unsigned.zip.sha256`

In Terminal, verify the download:

```sh
cd "$HOME/Downloads"
version=v0.2.0
archive="Covalent-${version}-macOS-arm64-unsigned.zip"
checksum="${archive}.sha256"
test -f "$archive" && test -f "$checksum"
shasum -a 256 -c "$checksum"
```

Continue only when the final command prints `OK`. Double-click the zip, then
drag `Covalent.app` into Applications.

Verify the installed app and both bundled executables:

```sh
app=/Applications/Covalent.app
test -d "$app"
codesign --verify --deep --strict --verbose=2 "$app"
codesign -d --verbose=4 "$app" 2>&1 | grep -F 'Signature=adhoc'
test "$(xcrun lipo -archs "$app/Contents/MacOS/Covalent")" = arm64
test "$(xcrun lipo -archs "$app/Contents/MacOS/covalent-node")" = arm64
```

Every command must exit successfully. `Signature=adhoc` and the two `arm64`
checks confirm the expected personal-use bundle.

These are first-install steps. If an older Covalent app already exists, do not
merge or replace its bundle until its backups pass Verify and a separate-folder
restore; personal ad-hoc update continuity must be proven for that exact
version pair.

## 2. Open it without weakening macOS

If macOS blocks this verified app, use its one-time exception:

1. In Finder, open Applications.
2. Control-click `Covalent`, choose **Open**, then choose **Open** again.
3. If macOS offers no Open button, try launching once, open **System Settings →
   Privacy & Security**, find the blocked Covalent message, choose **Open
   Anyway**, then confirm **Open**.

Do not run `spctl --master-disable`, do not turn off Gatekeeper, and do not
remove quarantine from Applications as a whole. A checksum or signature failure
is not a Gatekeeper problem: delete that download and get a fresh official copy.

## 3. Let Covalent start the Mac node

Launch Covalent and wait for status **Ready**. First launch automatically:

- creates private node data for this Mac;
- protects the node secret in Keychain;
- starts the bundled node on a private loopback address; and
- reconnects the app to that node.

The personal build saves its encryption keys in your login Keychain. If macOS
asks for access after a verified Covalent update, authorize the Covalent app you
just opened. Denied or locked access stops startup without replacing your keys.
Unlock the Mac, retry, and respond to the macOS Keychain prompt. Do not delete
the saved key to resolve an access error.

Nothing needs to be typed into **Service → Connect**. That form is a recovery
tool, not the Atlas pairing path, and a managed Mac returns to its bundled node
on refresh. Keep Covalent open and the Mac awake during the first transfer.

## 4. Pair a device and create a link

Open **Devices**, choose **Find Devices**, then **Pair with This Device** for a
discovered device. Alternatively, enter its reachable hostname or IP with port
8787 in **Device address** and choose **Pair Device**. Compare the confirmation code
on both devices and confirm it on each.

Open **Links**, choose your source folder and paired destination, then review
timing and deletion behavior. On a receiving Mac, **Choose Folder…** selects its
destination and accepts the incoming link. Use **Run Now** for your first transfer.
The Covalent menu bar item
shows each link's status and transfer actions.

Follow [Create your first one-way link](../getting-started-links.md) for additional
destinations, family collection folders, schedules, and deletion choices. Folder
links do not require a legacy backup or restore checkpoint.

For a Docker peer, allow UDP 8787 for pairing and link control. A Docker source
also needs TCP 8789 for authenticated file transfers. See the
[Docker network settings](../../packaging/docker/README.md) for the ports needed
by your chosen source and destination. Tailscale is optional; it must permit
those connections when used.

The older encrypted backup workflow remains documented in the
[legacy setup guide](../getting-started.md).

## Troubleshooting

- **Mac says app is damaged or cannot be opened:** repeat checksum and
  `codesign` checks. Redownload on any failure. Use the one-time Open flow only
  after both pass.
- **Status stays Offline:** choose Refresh. Confirm the app still contains
  `Contents/MacOS/covalent-node`; reinstall the complete app bundle if missing.
- **A device is absent on LAN:** confirm LAN discovery is enabled and UDP 8787
  is reachable, or enter its reachable address explicitly.
- **Tailnet pairing fails:** confirm both devices are online in the same
  Tailnet, use port 8787, and check the Tailnet policy permits UDP 8787.
- **Folder access was lost:** choose the folder again. Covalent never asks for
  broad disk access. See [Apple directory access](apple-directory-access.md).

More recovery decisions: [setup troubleshooting](../troubleshooting.md).
