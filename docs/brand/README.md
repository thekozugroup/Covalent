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

The UI images were captured on one isolated API 36 ARM64 emulator, using the
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
preview package, AVD, data and temporary APK were removed afterwards.

This is an implemented design direction, not a blanket accessibility or Material
compliance certification. Bonded owner control, import confirmations, real-device
roaming and the remaining screens still need end-to-end acceptance. Shared
background notification branding is not changed by this launcher/header pass.
