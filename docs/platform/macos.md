# Set up Covalent on macOS

Covalent supports macOS 15 or later on Apple Silicon. The app starts its own
private Covalent node automatically. When Atlas deployment becomes available,
pair it as a backup device; do not replace the Mac's local connection with
Atlas.

## Before you start

You need:

- an Apple Silicon Mac (`arm64`), not an Intel Mac;
- macOS 15 or later;
- about 15 minutes;
- after the replacement image is published, Atlas installed and claimed if you
  want an off-Mac copy; and
- one small, expendable folder for the recovery check.

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

Nothing needs to be typed into **Service → Connect**. That form is a recovery
tool, not the Atlas pairing path, and a managed Mac returns to its bundled node
on refresh. Keep Covalent open and the Mac awake during the first backup.

## 4. Complete the local first recovery checkpoint

Use expendable test data first:

1. Create a folder containing one small file whose contents you can recognize.
2. In Covalent, choose **New Backup**.
3. Choose the test folder, give the backup a clear name, and leave **Extra
   copies** clear. This first test is deliberately local-only.
4. Start the backup. Wait until the task completes and the snapshot appears in
   **Backups**. Do not quit while it is running.
5. Select that snapshot and choose **Verify**. Continue only when Covalent says
   the backup is intact.
6. Choose **Preview Restore…**, select a different empty folder, and keep the
   safest conflict policy for this first check.
7. Review the signed preview, choose **Restore**, and wait for **Restore
   Complete**.
8. Open the restored file and compare it with the original.

Checkpoint passes only when all four facts are true:

- backup completed;
- the snapshot is explicitly treated as a local-only evaluation;
- verification reported intact; and
- restore into a separate folder produced matching data.

This proves the Mac app and recovery path, not Mac-loss protection. Complete it
before adding any other device.

For a stronger source-loss drill, rename the expendable source folder after
verification, restore again into another empty folder, compare it, then put the
source back. Do not test by deleting irreplaceable data.

## 5. Optional after the checkpoint: prepare Atlas

Atlas deployment is currently blocked until a replacement signed immutable
`v0.2.0` image is published. Do not install the historical image. After the
replacement is available, follow the
[Atlas and Tailscale runbook](atlas-tailscale.md) to install and claim Atlas.
Claiming creates an owner-only directory containing `root.crt` and
`local-api-token`.

Use those files to trust and unlock the Atlas HTTPS console. Never enter the
one-time setup code in a browser or native app. Keep the original claim
directory on the trusted operator computer.

The Mac app does not need Atlas's token or certificate in its local connection
form. Device pairing carries and pins its own signed transport identity.

## 6. Optional after the checkpoint: pair the Mac with Atlas

Choose one network path:

- **Same LAN:** expose Atlas peer traffic on UDP 8787. In Covalent Settings,
  enable **Find devices on the local network**. Open **Devices**, choose **Find
  Devices**, then **Pair with This Device** for Atlas.
- **Tailnet:** put the Mac and Atlas on the same Tailnet and allow UDP 8787.
  Open **Devices**, enter Atlas's exact MagicDNS name with port 8787, such as
  `atlas.example-tailnet.ts.net:8787`, then choose **Use as Backup Device**.

Keep the Atlas web console's **Pair** tab visible. Compare every group of the
code shown on the Mac and Atlas. Continue only when they match, then choose
**Codes Match — Use as Backup Device** on both. Wait until the Mac lists Atlas
under **Connected storage devices**.

LAN discovery is only a hint; the code and signed identities create trust.
TCP 8443 serves Atlas HTTPS management. UDP 8787 handles device pairing and
encrypted replica traffic. After pairing, make a second backup with Atlas
selected under **Extra copies**, then run Verify again. That second result adds
Mac-loss protection to the local recovery proof.

## Troubleshooting

- **Mac says app is damaged or cannot be opened:** repeat checksum and
  `codesign` checks. Redownload on any failure. Use the one-time Open flow only
  after both pass.
- **Status stays Offline:** choose Refresh. Confirm the app still contains
  `Contents/MacOS/covalent-node`; reinstall the complete app bundle if missing.
- **Atlas is absent on LAN:** confirm LAN discovery is enabled and UDP 8787 is
  reachable, or use Atlas's Tailnet address explicitly.
- **Tailnet pairing fails:** confirm both devices are online in the same
  Tailnet, use port 8787, and check the Tailnet policy permits UDP 8787.
- **Backup cannot select Atlas:** pairing alone is insufficient. Wait for Atlas
  to report current capacity and appear as Connected.
- **Folder access was lost:** choose the folder again. Covalent never asks for
  broad disk access. See [Apple directory access](apple-directory-access.md).

More recovery decisions: [setup troubleshooting](../troubleshooting.md).
