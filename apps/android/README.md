# Android app

Android is Tier 1. The native Kotlin/Jetpack Compose project targets Android 17/API 37 with AGP 9.2.1, built-in Kotlin, and a restrained Material 3 surface. Its persistent floating action toolbar is limited to Pair, Backup, and Restore.

The production client will persist user-selected Storage Access Framework tree grants, expose revoked access, request Android 17 local-network access only when the user enables LAN discovery, and run resumable transfers through policy-compliant WorkManager/foreground execution. The foundation keeps primary actions visibly disabled until a real engine service is connected; it does not route production actions to a mock.

```sh
./scripts/check-android.sh
```
