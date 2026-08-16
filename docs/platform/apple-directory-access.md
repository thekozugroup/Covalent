# Apple directory access

## macOS Tier 1

Sandboxed builds use `NSOpenPanel` folder selection and persist security-scoped bookmark data in protected local settings. Each operation resolves stale bookmarks, balances every successful `startAccessingSecurityScopedResource()` call with `stopAccessingSecurityScopedResource()`, and uses `NSFileCoordinator` around coordinated reads/writes. Revoked or moved folders pause the job and ask the user to reauthorize.

## iOS Tier 2

The document picker grants user-selected directories only. Covalent persists permitted bookmarks where the provider supports them, coordinates access, pauses node checkpoints at expiration, and schedules supported refresh work. Reconstructing the original security-scoped archive request after process termination remains unaccepted Tier 2 work. Background tasks are opportunistic and bounded by iOS. The app does not access other apps' private data and does not claim full-device backup.

The shared Swift package contains the grant lifecycle helper; each native target supplies the picker and platform presentation.
