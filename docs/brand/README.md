# Interlink identity and native preview

Interlink is the dedicated companion for the headless photo appliance. Its own
minimal link mark, serif wordmark and launcher identity are separate from the
general Covalent app. The mark is authored as vectors, with no bitmap dependency.

`interlink-mark.svg` is the canonical standalone mark. The Android vector at
`apps/android/companion/src/main/res/drawable/ic_interlink_mark.xml` uses the same
path. The adaptive launcher has a forest background, white foreground and an
Android 13+ monochrome layer. Its 108 dp canvas keeps the glyph inside the 66 dp
safe area. [Android adaptive icon guidance](https://developer.android.com/develop/ui/compose/system/icon_design_adaptive).

`InterlinkTheme` uses native `MaterialExpressiveTheme` and
`MotionScheme.expressive()`. Android 12+ follows system dynamic colours; older
supported versions use explicit forest light/dark roles. `InterlinkScaffold`
shows the decorative mark and serif name without a stale Back button. Existing
native cards and adaptive endpoints remain shared with Covalent.
[Compose Expressive motion](https://developer.android.com/reference/kotlin/androidx/compose/material3/MotionScheme).

## Captured implementation

- `interlink-run-awaiting-source-light.png` and
  `interlink-run-awaiting-source-large-dark.png`: actual normal debug package,
  native receiving card and explicit example data. An expired source-owned run
  whose source cannot be confirmed shows Waiting for source and retained-progress
  guidance. The 200% dark view scrolls to the complete explanation. Source and
  destination are labelled; this fixture does not change the real Pictures link.
  [Run-status behavior and acceptance](../interlink-run-status.md).

- `interlink-camera-confirmation.png`: actual normal Interlink debug package,
  Expressive theme and stable-camera confirmation. Source/destination, counts,
  space and explicit remember are visible. Data is labelled as an example.
- `interlink-camera-connection-only.png`: a camera that cannot be remembered
  shows a plain explanation and Import once, without an inactive checkbox.
- `interlink-camera-confirmation-large-dark.png`: the same once-only camera
  review at 200% system text in dark mode. Content scrolls; actions stay visible.
  Fourteen native component checks pass, including reaching the complete camera
  explanation and original-retention note. These captures retain the normal
  Application, package and full native producers. Font/night restore, 1.0x motion
  and unchanged framework identities are checked. No real camera, import or
  Google Photos backup is represented.

- `interlink-drive-alert-settings.png`: actual notification permission control
  after granting through Android's real consent dialog. The normal Application,
  manifest and native producers are retained; the selected-Pi data is explicitly
  labelled as an example. The off/allowed control updates without a shell grant.
- `interlink-drive-alert.png`: actual Android notification using the production
  builder and explicit example drive/count/size/destination/space. It has no
  approval action; tapping opens the real launcher for fresh review. Separate
  actual background and native-owner tests pass, but the shown source is
  synthetic. See `../interlink-background-alerts.md` for exact acceptance.

- `interlink-first-launch.png`: current normal companion package on a fresh
  API37 ARM64 emulator, using the real Interlink Application and launcher.
  Get started is visible and the foreground host remains off until explicit
  opt-in. No example-data override, native-producer exclusion, account or
  pairing is used. The same installed APK subsequently passes both permission
  recovery routes and automatic host startup after one real reboot; see
  `../interlink-startup-recovery.md`. This capture does not prove real pairing
  or cloud backup.

- `interlink-import-confirmation.png`: current actual native confirmation with
  synthetic source, count/size, named destination and usable space. A new source
  defaults to once; remembering is an explicit checkbox. Folder details reveals
  the fixed relative path. No import or RPC runs in the visual fixture.
- `interlink-import-confirmation-large-dark.png`: the same dialog in dark mode
  at200% system text. Content scrolls; confirmation/dismiss remain reachable.
  Ten native component tests cover the current dashboard and import flows,
  including optional details, double submission, explicit remember, busy/offline
  controls, pending-source pause and large text. Font/night restore and unchanged
  boot/framework identities are recorded in the private platform audit.

- `interlink-notification-preview.png`: actual Android notification using the
  production builder and merged Interlink icon/title resources. Its visible
  example-data label is synthetic; no sync engine or cloud backup is running.
  Tapping it from HOME opens this fixture's own launcher.

- `interlink-storage-preview.png`: actual native storage card with synthetic
  source counts, destination and protected space. It is read-only; the shown
  counts are cached and do not approve imports or establish transfer/backup.

- `interlink-dashboard-light.png` / `interlink-dashboard-dark.png`: current
  selected-Pi dashboard with synthetic approved status.
- `interlink-dashboard-offline.png`: the same selected Pi retained while
  connection status is unavailable.
- `interlink-owner-approval.png`: the current approval-required dashboard with
  example phone/Pi comparison fingerprints. No real approval or live node.
- `interlink-dashboard-large-actions.png`: the current dashboard at200%font,
  scrolled to both accessible actions. Three actual native component tests cover
  retained selection/stale-status removal and large-text action behavior.
  The third also checks both fingerprints and their removal after approval.
- `interlink-preview.svg` / `.png`: vector identity board.
- `interlink-ui-preview.png`: native light-mode screen.
- `interlink-ui-preview-dark.png`: native dark-mode screen.
- `interlink-ui-preview-large.png`: native dark-mode screen at 200% font size,
  scrolled to show the stacked source/destination and link actions.

The dashboard/link images were captured on an isolated API 36 ARM64 emulator, using the
actual production theme/scaffold and folder components with synthetic saved,
offline Pictures data. The visible example-data label belongs only to the debug
`InterlinkPreviewActivity`. It has no counterpart in release sources.
The earlier `interlink-ui-preview*` images retain the previous links-only design
for comparison. Current dashboard behavior is in `docs/interlink-dashboard.md`.

For this visual stage only, a temporary Gradle/manifest fixture used the separate
`life.michaelwong.interlink.preview` identity and generic Android Application,
omitted the real launcher and native engine producers, and launched the preview
explicitly. The committed normal companion build still requires its JNI,
maintained worker and guardian producers and package checks. These images prove
rendering only, not pairing, transfer, cloud backup or a signed release package.
The emulator boot/framework identities stayed unchanged during capture. Its
preview package, AVD, data and temporary APK were removed afterwards. The later
notification capture uses a separate owned API36 ARM64 emulator with the same
visual-fixture limits; its boot/system-server/surface-flinger identities remain
unchanged and animations stay at1.0x.

The shared foreground and transfer builders use a monochrome notification icon.
Interlink's `src/brand/res` overlays the shared app-name, channel/title and icon
resources in both debug and release build types. Covalent retains its own mark
and app name. The service keeps its existing channel ID and stop action, opens
the installed app's own launcher on tap, and uses immutable explicit intents.
Readiness never means an active transfer or completed Google Photos backup;
unavailable folder access takes priority over readiness on repeated starts.

This is an implemented design direction, not a blanket accessibility or Material
compliance certification. Bonded owner control/import confirmations are wired in
source. Normal-package local startup now passes its separate qualification.
Background alerts now pass normal-package local qualification. Actual phone/Pi
control and alerts, real-device roaming and the remaining screens still need
end-to-end acceptance.
