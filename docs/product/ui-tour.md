# Covalent UI tour and audit checklist

Status: updated 2026-08-28. This is the operator's periodic visual-audit guide.
Descriptions explain what each supported screen should look like; evidence is
current only when tied to the exact source SHA being released.

## What is supported

The release surfaces are:

- Android phone app, including either a separate Covalent server or the server
  running on the phone.
- Apple Silicon macOS app only (arm64). The app has a normal window and a
  menu-bar item/menu.
- Docker/Unraid web console, served by the Covalent node.

iOS is informational/Tier 2 only. It is not a supported release client and its
screenshots or tests cannot approve a release. Windows has no client or
packaging.

## Evidence rules

### Source-derived description

The screen maps in this document are derived from these current source areas:

- Android: CovalentApp.kt, CovalentViewModel.kt, and strings.xml under
  apps/android/app/src/main.
- macOS: CovalentMacApp.swift, MacRootView.swift, and the Mac*View.swift files
  under apps/apple/Sources/CovalentMac.
- Web: index.html, app.css, app.js, and the pairing, restore, and backup flow
  modules under packaging/web.

These descriptions explain intended layout and behavior. They do not prove that
a built app renders correctly on a real screen.

### Fresh headed evidence

For each release, capture the exact source SHA, device/browser details, and
redacted artifacts. A green unit test, static screenshot, accessibility dump,
or old result bundle is not fresh visual evidence.

### Current exact-source macOS evidence

Fresh headless evidence was captured on 2026-08-28 for v0.2.0 (build 2000) on
Apple Silicon, macOS 26.5.1, and Xcode 26.6. The checked release-input file set
is rooted at base commit `dafca8efebaf904ed886d48ba8371b0fde53af56` and has
source-manifest SHA-256
`740d520afb21af4238bbfff5051b6d7e183b3d90cfc42c63dfae4b5850adadf9`.
The same digest was confirmed after testing and archiving.

- `artifacts/release-candidate-0.2.0-local-macos-20260828/swift-test.log` records
  91/91 passing Swift tests.
- `artifacts/release-candidate-0.2.0-local-macos-20260828/real-daemon-integration.log`
  records 1/1 passing real-node backup, verify, and restore integration.
- `artifacts/release-candidate-0.2.0-local-macos-20260828/release-archive-unsigned.log`
  and the matching inspection log record a successful v0.2.0, build 2000 Release
  archive with a thin arm64 app/helper and no x86_64 slices.
- Additional signing, entitlement, source-manifest, and stability logs are in
  `artifacts/release-candidate-0.2.0-local-macos-20260828/`. These artifacts
  are intentionally ignored and are not GitHub documentation links.

This is not headed UI evidence. The login session was locked, and the UI gate
failed closed with exit 75 as recorded in
  `artifacts/release-candidate-0.2.0-local-macos-20260828/headed-ui-blocker.log`.
No bypass was attempted. Menu-bar reachability, the exact three named UI tests,
system accessibility audit, default/narrow window layouts, and fresh screenshots
remain pending until an unlocked headed session is available.

Known historical artifacts at this review:

- artifacts/android-dedicated-device/ has Android screenshots/logs from
  2026-08-16. They are stale against current source. The old Home capture has
  the floating action bar over backup content; the pairing-code image is blank.
- artifacts/remediation/browser-pairing/ has 2026-08-16 desktop pairing
  screenshots and accessibility dumps only. It does not cover current Backup,
  Restore, Settings, mobile-width, or first-run surfaces.
- No current macOS headed screenshot/result bundle is present. The fresh
  evidence above is headless only. The old
  artifacts/release-candidate-0.1.0-4a2fa56/ macOS log ends in BUILD INTERRUPTED
  and is not a pass.
- No real Unraid-host install, upgrade, restore, or visual evidence is present.

Do not label any of these artifacts as current in a release report.

## Android phone app

The current headed API 37 gate now passes 57/57 tests on the `Covalent_API_37`
emulator and includes first-launch capture evidence. This proves the current
gate path only; the full journey matrix (serial, portrait/landscape, light/dark,
reduced motion, and text scale) remains pending. Do not use the historical
captures listed above as release proof, and rerun the full matrix after any
uncommitted UI changes land.

The app uses a Material 3 top bar, a scrollable content column, and a centered
primary-action toolbar on Home. The content column is capped in width and forms
use large outlined fields and cards. Text, icons, and controls have Compose
semantics for headings, buttons, checkboxes, radios, switches, and state
descriptions.

### Setup — Connect your backup server

What it looks like: a single scrollable form headed Connect your backup server,
with two explanatory choices (Server on your network and Setup link), then
fields for device name, server address/setup link, and access token. HTTPS trust
is below: choose a CA certificate or enter an exact certificate fingerprint. A
prominent Connect button ends the form.

Primary actions: open a covalent://connect link or enter an address, enter the
token, optionally enroll the exact CA/pin, then Connect. A setup link can only
prefill the address; it never supplies a token or certificate. The app asks for
local-network permission when needed.

Audit states:

- First run: empty fields. Connect stays available so it can focus the first
  invalid field and show inline guidance; no request is sent until every
  required value is valid, and the button is disabled while checking.
- Invalid name/address/token/certificate: field-level supporting text.
- Insecure remote HTTP: blocked with an explanation; HTTP is allowed only for a
  server running on this phone.
- Permission denied: no request is sent; the form explains that local access
  was not granted.
- Connection checking: button reads Checking…; successful verification leads
  to Home. An unauthorized refresh returns to Setup.
- Already connected: an outside setup link is ignored; changing servers is a
  deliberate Settings action.

### Home — Your copies. Your devices.

What it looks like: a heading and privacy-focused subtitle, followed by a
connection card. The card shows ready/checking/unavailable/not-connected state,
server name/version when ready, last verification time when stale, and a
Reconnect button when needed. Completed or active transfers appear next. The
Remembered backups list contains one card per backup, with snapshot count,
local-only or explicitly selected extra-copy devices, provider reachability,
Verify, and Change extra copies. A centered action toolbar offers Pair, Backup,
and Restore; Settings is in the top-right.

Primary actions: reconnect/refresh, verify a backup, change copies for the next
backup, or open Pair/Backup/Restore. Transfer cards provide Pause, Resume,
Retry, or Cancel as appropriate. Cancel opens a destructive confirmation.

Audit states: no backups yet; backup remembered but no snapshot; provider
connected/offline/unknown; queued/running/paused/completed/needs attention/
cancelled transfer; and connection stale/disconnected. Confirm the floating
toolbar never covers the last card, especially in landscape and at 1.3 text
scale.

### Pair — Pair a device

What it looks like: a Tailscale name/IP field, a Find devices action, discovery
results, and a secure pairing card. The card shows incoming/outgoing direction,
peer name, a large short comparison code, expiry, current state, and
confirmation/cancel action. Advanced pairing reveals the Invite a device and
Join a device roles, then expands into signed JSON exchange cards with Copy and
Share buttons.

Primary actions: discover nearby devices, enter a Tailscale address, start or
accept a pairing, compare identities/roles/code on both physical devices,
confirm one side, exchange the updated signed session, finalize both sides, and
Use as backup device when the signed provider transport is ready.

Audit states: searching; no candidates; LAN permission denied; outgoing request
waiting for peer; incoming request; confirmed here/waiting for peer; complete;
expired/cancelled/failed; and provider certificate/capacity mismatch. Advanced
pairing must not expose a way to type an arbitrary certificate or silently trust
an address. Technical JSON may be copied/shared only from the explicit recovery
section.

### Backup — Create a backup

What it looks like: a form with Create new backup or Add to an existing backup,
Choose source folder, backup name, and an Extra copies section. Each provider is
an outlined selectable card showing friendly name/address, reachability, roles,
fingerprint, and capacity. A review card summarizes source, exclusions, folder
access, exact copy count, and changes to an existing replica set. Queue backup is
the final action.

Primary actions: choose a source through Android’s folder picker, name the
backup, select zero or more eligible providers, review the exact copy impact,
and Queue backup. Zero providers means an intentional local-only copy.

Audit states: no source; invalid name; no eligible providers; reachable,
offline, unknown, expired, or insufficient-capacity provider; queued/running/
paused/completed/failed/cancelled transfer; and Android folder permission
revoked. Providers that are not currently verified and sufficiently provisioned
must be visibly disabled, not silently replaced.

### Restore — Restore safely

What it looks like: choose a remembered backup and latest snapshot, select one
of three conflict policies (Stop without writing, Skip existing files, Keep
both), choose a writable restore folder, then Preview restore. The signed path
preview card lists the exact path range and each entry’s kind/action, with Show
next paths for pagination. Queue restore is enabled only when the plan and
target inventory remain valid.

Primary actions: choose an authorized target, preview, inspect every page, and
Queue restore. Replace is intentionally unavailable on the general Android
folder path because crash-safe replacement cannot be guaranteed.

Audit states: nothing to restore; target unavailable/not writable; preview
loading; signed preview with safe actions; expired or changed target inventory;
unsafe path blocked; and queued/running/paused/completed/failed restore. Check
that no path traversal or absolute escape is presented as executable.

### Settings

What it looks like: radio cards for Separate server and This phone, the Store
backups on this phone provider panel, Nearby discovery switch, and safe
settings export/import controls. The phone-provider panel shows status,
used/reserved/available storage, measured key protection (secure hardware,
StrongBox, software-only, or unavailable), maximum GB, keep-free GB, and an
optional Let nearby devices find this phone switch.

Primary actions: choose the separate server or local phone node, enable/disable
phone storage, set capacity/free-space limits, toggle discovery, export safe
settings, import a JSON file, and change server connection. Import first shows
name/discovery/remembered-backup changes; a reduction in remembered backups
requires explicit destructive confirmation.

Audit states: phone provider unavailable/locked/not installed; permission
denied; invalid capacity; enabled/running; software-only protection warning;
LAN on/off; empty or populated folder grants; valid import; unsupported schema;
and import that removes remembered backups. Verify private keys, credentials,
and folder permissions are never shown as exported/imported data.

### Android accessibility and responsive checks

- Use the headed API 37 emulator Covalent_API_37 and the required
  MobileMCP/mobilecli flow. Never use the Bloop emulator.
- Capture portrait and landscape, light/dark theme, and 1.3 text scale.
- Check TalkBack names for headings, toolbar actions, provider checkboxes,
  conflict-policy radios, switches, progress, error text, and destructive
  confirmations.
- Scroll from the first field/action to the final action without clipping.
- Verify the centered action toolbar does not cover Home content or become the
  only way to reach a primary action.
- Do not include tokens, setup JSON, certificates, or unredacted device IDs in
  screenshots.

## Apple Silicon macOS app

The macOS app is arm64 only. The main window is a NavigationSplitView with a
sidebar and detail area, a toolbar, system sheets/dialogs, and a persistent
active-task bar. It also installs a MenuBarExtra with a status icon and menu.

### Main window and menu bar

The sidebar contains Overview, Backups, Devices, and Settings. Its footer shows
service state and the connected device name. The detail area is a scrollable
page; the toolbar offers Refresh and New Backup. Active backup, verification, or
restore work appears in a bottom task bar with progress plus Pause/Resume and
Cancel.

The menu-bar icon changes for starting, ready, authorization required, and
offline. Opening it shows Open Covalent, service summary, active-job controls,
New Backup, Restore Latest Backup, Refresh Status, Settings, and Quit Covalent.
The app menu also provides New Backup, Pair Device, Overview/Backups/Devices
shortcuts, Refresh, and Connect.

Audit the menu-bar item itself, not only the window: its accessibility label is
Covalent, its value states service status, and all disabled conditions make sense
when unauthorized or a job is active.

### Overview

What it looks like: a large greeting (Welcome to Covalent or “device is
protected here”), privacy subtitle, prominent New Backup button, optional
connection callout, three metric tiles (Backups, Extra copy devices, LAN
discovery), Recent backups, and Built around your choices safeguards.

Primary actions: Connect/Try Again, New Backup, See All, and open a recent
backup. Empty state is a clear No backups yet card with Create Backup.

Audit starting/authorization-required/offline/ready callouts, zero and
non-zero backup lists, long device names, narrow windows, and the lower
safeguards row above the Dock boundary.

### Backups and backup detail

What it looks like: an empty state when none exist; otherwise a two-pane view
with date-grouped backup list on the left and selected-backup detail on the
right. Detail shows name/date, optional repair warning, size/items/new chunks/
deduplicated metrics, copy placement, Recovery actions, and collapsed
Technical details.

Primary actions: New Backup, select a backup, Verify, Verify and Repair, Preview
Restore, Check Backup, and change extra copies for the next backup. Repair has
a confirmation explaining that only already-selected intact devices are used.

### Backup, restore, pairing, and import sheets

- New Backup sheet: choose new/add-to-existing, name, authorized source folder,
  exact extra-copy toggles, and a review section. The final button is Create
  Backup or Add to Backup.
- Restore setup sheet: choose authorized destination and conflict policy; a
  replacement policy has an extra confirmation before the no-write preview.
- Restore Preview sheet: a table of Source, Destination, and Action, item count,
  signed target-inventory warning or confinement message, Cancel, and Restore.
  Replacement requires another destructive confirmation. Restore Complete then
  reports files, folders, skipped items, and bytes.
- Secure Pairing sheet: segmented Invite/Join flow, short-lived invitation,
  copy/share exchange, roles, code comparison, two confirmations, and final
  signed connection. Direct network pairing is available from Devices.
- Import Device Settings sheet: choose a JSON file, review device name/LAN
  discovery/remembered backups, and Replace Settings. Identity keys, backup
  keys, storage credentials, and folder permissions are excluded.
- Setup/connection sheet: server address, protected token with Show/Hide, token-file
  chooser, device name, LAN discovery, exact CA chooser, Cancel, and Connect.

### Devices

What it looks like: Your backup network header with Find Devices; Tailscale
hostname/IP entry; nearby candidate cards showing source, endpoint, protocol
compatibility, and Pair with This Device; connected storage-device cards with
certificate prefix, Connected status, and a Disconnect/Revoke menu; expandable
Advanced recovery; and a trust-explanation callout.

Primary actions: discover, pair by LAN/Tailscale, complete code-confirmed
pairing, connect a signed provider, disconnect, revoke, or use offline signed
files when direct pairing is unavailable. Revoke is destructive and explains
that existing encrypted copies remain.

Audit empty/no-candidate, LAN-off, incompatible, in-progress, complete,
offline, revoke-confirmation, and advanced-recovery states. Tailscale entry is a
manual one-time address path; Tailscale does not enumerate devices for the app.

### macOS accessibility and responsive checks

- Run only on Apple Silicon with the exact arm64 build in a headed unlocked
  session; no current macOS headed evidence exists yet.
- Check default and narrow windows, sidebar/detail navigation, keyboard
  shortcuts, VoiceOver labels, sheet focus, confirmation dialogs, and menu-bar
  navigation.
- Test long paths and IDs with middle truncation, and ensure technical details
  are behind disclosure rather than leading user copy.
- Enable Reduce Motion: the active-task bar should not slide/animate, and no
  action should depend on motion.
- Check the Dock does not cover the sidebar service footer or bottom task bar.

## Docker/Unraid web console

The console is one responsive page with a centered 960px maximum width. It uses
rounded white/dark cards, large pill buttons, outlined inputs, tabs, and a
mobile breakpoint at 700px. It follows the system light/dark color scheme and
disables transitions/scroll animation for reduced motion.

Current web evidence (2026-08-28): 61/61 web tests passed with zero console
errors or warnings. Fresh desktop and mobile light/dark, reduced-motion
screenshots are in the ignored local directory
`artifacts/ui-audit-0.2.0-local-20260828/web/`. They are inspection evidence,
not release assets or GitHub links.

The tab bar is one keyboard stop. Left/Right Arrow wraps through tabs, Home
moves to Pair, and End moves to Settings; focus, selection, and the one visible
tabpanel change together. All buttons and form controls retain a 44px minimum
target. Primary-button text uses a theme-specific contrast token so both the
dark theme's light blue accent and its lighter hover state keep readable dark
labels.

### Header, status, and claim-token onboarding

The header says Private distributed backup, Covalent, and Tier 1 node. The hero
card shows This device, node name, service state, protocol, and Refresh status.
The Local access card contains the local API-token password field and Unlock
console action. The token is entered from the owner-only covalent claim output;
it is held only in the current browser tab. The console does not accept a setup
code.

First-run appearance is loading/connecting until the node responds. Without a
token, data-changing operations remain locked. Invalid/expired token,
unauthorized, certificate, protocol, offline, capacity, and job errors appear
in a live toast with plain-language recovery copy; technical details are
collapsed below it.

### Pair tab

The Pair tab begins with Pair a device, a Look for devices button, a live search
status, candidate list, and manual Tailscale name/address form. A pairing card
appears after starting or receiving a request, showing peer, direction, large
comparison code, expiry, state, Confirm, and Cancel/Dismiss.

An advanced disclosure provides signed invitation creation, invitation
acceptance with role checkboxes, session/code confirmation, identity/role/code/
signature summary, finalize buttons, clear exchange, and a final Use as backup
device card. This is the manual recovery path, not the normal direct pairing
path.

Audit no candidates, discovery off, incoming/outgoing, waiting, confirmed,
complete, failed, expired, cancelled, settled-dismissed, mismatched protocol,
and provider capacity/fingerprint failure. Confirm disabled/enabled states and
that raw JSON appears only inside the explicit advanced exchange.

### Backup tab

The Backup tab has a heading, source directory (default /source), backup name,
snapshot ID, Remembered backups card, and Backup devices fieldset. Provider rows
show friendly name, immutable ID, usable/allocated/quota capacity, or a clear
reason they are unavailable. A notice says selection is explicit and that an
empty selection is local-only. Start backup and Try again/Confirm receipt
controls appear in the status area.

Audit locked versus unlocked list, empty list, selected/unselected providers,
offline/unknown/full/stale capacity, starting/running, completed receipt,
acknowledgement still needed, retry, and failure. A receipt that has not been
acknowledged remains in bounded, origin-bound durable browser storage across a
tab or browser restart. Covalent verifies its strict schema, future-clock bound,
server context, and SHA-256 integrity before retrying the exact job, stores no
access token, and removes the receipt only after the server returns the
acknowledgement `204`. Seven days is a review marker, not destructive expiry: a
valid record remains resumable for as long as the server may retain it. An
exclusive same-origin Web Lock covers reconciliation, request, rendering, and
acknowledgement so concurrent tabs cannot strand different receipts. Corrupt,
changed, future-dated, or wrong-origin state blocks a new backup rather than
silently discarding a possibly unacknowledged server result.

### Restore tab

The Restore tab is a compact form for Backup ID, Snapshot ID, writable target
(default /restore), and conflict policy. Preview restore reveals a result card
with signed entry count, target root, JSON-like path listing, Previous/Next
page, Discard preview, a review checkbox, and a disabled-until-checked Restore
this preview button.

Audit no-write preview, pagination, discard, target/inventory change, unsafe
path, conflict policies, and completed/failed restore. Verify that only the
displayed signed target can be executed and that buttons do not activate before
the review checkbox.

### Settings tab

The Settings tab has a heading showing LAN discovery On/Off/Unknown, two cards
for Download settings and Import settings, and a container-network explanation.
Export includes only device name, discovery preference, and remembered backups;
import requires pasted JSON plus an explicit confirmation checkbox. The network
note describes bridge TCP 8443 for HTTPS management, UDP 8787 peer traffic,
package CA enrollment, optional host networking, and optional Tailscale sidecar.

Audit export/download, malformed or unsafe import, confirmation unchecked,
successful import, discovery state refresh, mobile one-column layout, and dark
mode. Do not put a bearer token, CA private material, or authenticated URL in
an exported screenshot.

### Web accessibility and responsive checks

- Test desktop and <=700px mobile widths, light/dark system themes, keyboard
  focus, skip link, tab/tabpanel semantics, live status/toast announcements,
  disabled controls, long paths/IDs, and reduced motion.
- Check every primary action is visible without horizontal scrolling and that
  pre blocks scroll without widening the page.
- Confirm plain copy never displays a raw server/browser error as the headline;
  technical details remain a deliberate disclosure.
- Verify token unlock is tab-local and cleared after a failed attempt.

## Periodic owner audit

Use this short cadence after every release and after a major UI change:

1. Record source SHA/tag, artifact directory, Android API/serial, macOS
   version/arm64/window size, browser/viewport/color/reduced-motion settings,
   image digest, and Unraid host.
2. Walk first run/setup, pairing, one local-only backup, one explicit provider
   backup, verify, restore preview, restore execution, settings export/import,
   and one failure/retry path.
3. Capture initial, working, blocked, failed, and recovery states for every
   supported surface. Redact secrets and device identifiers.
4. Check light/dark, narrow/wide, large text/zoom, keyboard/screen reader,
   reduced motion, focus order, and no clipped primary actions.
5. Record exact artifact paths and verdicts. Mark old or source-only evidence as
   stale/non-proof; do not carry a prior release’s visual pass forward.

Copy this header into each audit record:

    Audit date:
    Source commit/tag:
    Auditor:
    Android device, OS/API, serial, and headed emulator:
    macOS version, Apple Silicon architecture, and window size:
    Browser, viewport, color scheme, and reduced-motion setting:
    Docker image digest and Unraid host:
    Artifact directory:

End with: exact SHA, fresh artifact paths, passed journeys, open visual or
accessibility gaps, release blockers, and owner decisions required.

## Source and validation references

- Android exact-device lane: scripts/android-api37-device-test.sh using
  Covalent_API_37; never use Bloop.
- macOS UI lane: apps/apple/Scripts/macos-ui-test.sh in a headed session.
- Web/Docker/Unraid contracts: validation-matrix.md and the web tests under
  packaging/web/tests.
- Unraid validation remains separate from source-derived UI review: validate the
  template, then perform real-host install/upgrade/selected-share/boot-share
  and signed-preview restore drills.

Never commit, publish, or attach raw tokens, setup codes, private certificate
material, signed invitations/sessions, bearer-authenticated URLs, or
unredacted device identifiers.
