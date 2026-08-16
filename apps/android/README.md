# Android app

Android is Tier 1. The native Kotlin/Jetpack Compose project uses a restrained Material 3 surface and a persistent floating action toolbar limited to Pair, Backup, and Restore.

The production client will persist user-selected Storage Access Framework tree grants, expose revoked access, and run resumable transfers through policy-compliant WorkManager/foreground execution. The foundation keeps primary actions visibly disabled until a real engine service is connected; it does not route production actions to a mock.

```sh
./scripts/check-android.sh
```
