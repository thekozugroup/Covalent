# Import review reminders while the screen is closed

Interlink checks the selected Pi while its existing opted-in native sync host
runs. An attached SD/USB drive or compatible camera with a complete, usable preview can produce one
private Android notification. It shows the eligible file count, total size,
selected Pi, source labels, destination and available space. Tapping opens the
real Interlink launcher. It never approves an import: the user still reviews a
fresh source and explicitly chooses once or remember in the app.

The background reader uses the same authenticated loopback API and full paired
identities as the dashboard. Native transport resolves the Pi by identity over
LAN/Tailscale; notification data contains no IP address, credential, attachment
capability or executable import action. The selected Pi and protected generation
are rechecked before publication. Generic public lock-screen content hides names,
counts and destinations; private details follow Android's notification settings.

Only manual, connected sources with a ready preview, nonzero eligible
count, available service and sufficient protected space qualify. Cameras require
an explicit version2 protected context. Unavailable, scanning, incomplete and
automatic sources do not qualify. Current-bond unresolved requests suppress
new prompts for that same service kind. An accepted import, withdrawn request or accepted pause suppresses
its old preview; a new preview or attachment can request review again. This is
reminder policy, not import authorization or a Google Photos backup receipt.

There is one process-scoped I/O worker. The native service supplies its exact
owner token; stopping/reaping that owner cancels scheduled checks and removes
its notification. A late response cannot publish after stop, an owner restart
or a changed selection. Old callbacks cannot stop a newer owner. The worker is
serial and cancelled tasks leave its queue. It waits 30 seconds after each
completed poll; bounded API reads use 5-second connection/read timeouts. An
in-flight read may finish after cancellation, but its result is discarded.
No extra Android service, dependency or permission is introduced.

The per-Pi import alerts control reflects actual app/channel permission and
provides Allow import alerts and Notification settings. Existing channel IDs and
owner permission choices are preserved. Permission is requested
only after the user's tap. Returning from Android settings refreshes that
state. Denied/blocked permission prevents background API reads. Notification
failures cannot terminate the sync owner or polling schedule. An active notice
can update without repeated sound; user dismissal suppresses the same preview
for the process lifetime. An unread notice hidden during a temporary outage
returns silently when fresh data is available. A process restart can remind
again about an unhandled preview.

These are local reminders, without a push service. Android can suspend CPU or
network work in Doze, stop an app, or deny foreground starts. A strict immediate
alert deadline is not promised, and opening the app remains the recovery route.
No battery exemption, wake lock or global power-policy change is added.
[Android notification permission](https://developer.android.com/develop/ui/compose/notifications/notification-permission)
and [Doze guidance](https://developer.android.com/training/monitoring-device-state/doze-standby).

Background notices for uncertain outcomes, remaining remote
settings and actual Pi/Solstice LAN/cellular acceptance remain separate work.
Originals stay on their source; exact Google Photos confirmation is still
required before cleanup. No source deletion, physical media import, cloud
backup, production signing or gold-image acceptance follows from an alert.


## Local verification — 2026-11-01

All 262 JVM tests pass, including 12 new alert/timeout checks: strict eligibility,
accepted/withdrawn/paused preview suppression, nonce churn versus a new preview,
count overflow, changed selection, old-owner/same-owner restarts, dismissal,
permission denial, notification failure and timeout/recovery. The full normal
companion build runs both native producers and passes debug/minified unsigned
release/AAB, native provenance/alignment/budgets, SBOM and lint (zero errors;
11 debug/12 release warnings). The release APK grows 11,484 bytes to 29,522,722.
A frozen final full-package pass takes 158.080s after the initial 547.249s native
build; later changes are limited to the instrumentation fixture. Runtime APK
SHA256 is dd9520b70afc9fbdd4f3796e0f2189b38bde22f7079fd6c01c03ad0acdd4312a.

The actual API 37 ARM64/4-KiB-page normal Application/package passes both permission
recovery routes in 63.166s and one real reboot in 57.118s. Native host readiness
returns without an Activity launch or direct service command; 20.919s of stable
observation, identical APK/UID/identity hashes and exact forward cleanup pass.
Real notification consent moves the actual control from off to allowed, without
a shell permission grant. Four background tests pass in 13.884s: the real native
host starts one observer, a synthetic drive notice posts without resuming an
Activity, its owner stop removes it, and its tap opens the real launcher. Private
public-version text, immutable intents and absence of approval actions are
checked. Sixteen component tests pass in 29.654s, including the new 200% text
permission control. No ignored/skipped test is presented as acceptance.

The final automated journey repeats real notification consent and all 20 native
owner/notification/component checks together: 20/0 in 54.485s overall (41.574s
instrumentation). It verifies the exact installed app/test APKs, same boot and
framework processes, unchanged three identity hashes and UI-dump cleanup.
Final test APK SHA256 is
83ce0c6814507b972ac3cafcae2ac980e409d1647c7181d7a6bfe4aedcf23fea.
The harness refuses physical/foreign guests and mismatched startup proof; its
three guard/parser regressions pass. CI runs it after the existing permission
journey, with neither earlier gate skipped. Workflow YAML parses successfully.

The original fixture failed: it called stopService 15ms after Android accepted a
foreground start, before foreground promotion. The guest trace explicitly says
it was brought down while waiting for start foreground. The corrected fixture
waits for actual native API readiness before isolating a synthetic notice; the
same runtime APK passes. That failure and the successful rerun remain distinct.
The first combined journey passed 19/20: its fixture could finish cleanup before
the launcher it requested had resumed. It now observes the real resumed Activity
before waiting for native readiness and closing it. The final combined run uses
the same runtime APK; no deadline or assertion was relaxed. Both failures remain
in the private audit with their original logs.
An initial static route extractor expected the previous call signature; its new
exact timeout forwarding pattern and negative fixtures pass. No assertion,
foreground deadline, native producer or security requirement is bypassed.

Canonical images: `brand/interlink-drive-alert-settings.png` and
`brand/interlink-drive-alert.png`. They use explicitly labelled example Pi/drive
data in the actual normal native debug APK. No account, paired transfer, real
card or cloud backup is implied. Bounded original logs, exact app/test/source
hashes and cleanup live in the private platform audit
`artifacts/validation/2026-11-01-background-drive-alerts`.

The physical Pi was independently rediscovered with its known identity,
bcm2712/API 36 and unchanged boot/identity hashes. It still runs 0.2.3:
four native services are running, but camera/import services are absent. Its
incoming runs remain incomplete/running with disconnected peers. This is a
read-only observation, not migration admission or an installed alert test. The
existing updater idle barrier and Pictures journal remain protected. Nothing
was installed, paused, retried, restarted or deleted on the Pi.

Hosted baseline public1811e18 run41292423124 passes Android foundation, all86
general Android tests and the SAF journey. It then fails during the companion
permission harness's Settings tap (ADB exit20); both container image scans also
fail, so the full software gate remains failed. Private ef8bff1 run41292634020
passes both platform jobs. These are prior-head results; the new feature's
hosted CI must qualify independently.
