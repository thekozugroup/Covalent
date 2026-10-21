# Interlink Android companion

Focused Pi photo-backup companion, installed as `life.michaelwong.interlink`.
It shares Covalent's authenticated pairing, protected identity, SAF folder grants,
foreground node, settings convergence, recovery and transfer engine. It has its
own private state and ports (18887/18889), so it does not replace a Covalent
installation or reuse its identity without an explicit future migration.

The UI uses Compose Material 3 Expressive (`1.5.0-alpha29`), expressive motion,
dynamic light/dark colours, the existing logo and a serif title. The main screen
shows paired source/destination links and per-link settings. It omits the
general backup-server and archive-recovery flows. Deletion/restore policy
switches are unavailable in the appliance companion. Original files stay on
the phone; Google Photos controls cleanup on the Pi.

Build a development APK from `apps/android`:

```sh
./gradlew :companion:assembleDebug
```

The build requires the same pinned NDK, Rust and Go toolchains as Covalent,
and rebuilds both JNI and the restricted sync worker. The output is
`companion/build/outputs/apk/debug/companion-debug.apk`. This is a **debug APK**,
not a production-signed release. Publishing needs a durable production signing
identity, companion license/SBOM checks, physical-device testing and release automation.

Pair once with the Pi, compare the confirmation code on both devices, choose
Pictures and approve the receiving folder on the Pi. Do not select DCIM until
the Pictures test passes. Both devices need Tailscale connected to the same
tailnet for roaming. An address is a route; the signed paired identity remains
the trust anchor. Discovery never automatically trusts a device.

Boot/update receivers reconnect only explicitly enabled nodes after unlock.
Android can still block background starts after force-stop or with restrictive
battery settings. The foreground notification remains necessary on the phone.
Physical phone/Pi restart and cellular handoff remain acceptance gates.

Development validation (2026-10-20): 205 shared unit tests, debug/release lint,
and minified API 36 pairing, named destination, SHA-256 verified transfer and
successful transfer commit. Reboot and in-place package update restored the
enabled foreground service. A new file transferred after update without opening
the Activity, retaining identity and source files. Fast address input, keyboard,
rotation and 200% text were checked. These tests use synthetic media and a
QA-only debug certificate; they do not prove Google Photos backup or real
phone/Pi roaming. API 37 companion device testing remains open.

References: [Material 3 Expressive](https://android-developers.googleblog.com/2025/05/androidify-building-delightful-ui-with-compose.html),
[storage grants](https://developer.android.com/training/data-storage/shared/documents-files).
