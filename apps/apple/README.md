# Apple apps

- `CovalentMac`: Tier 1 native SwiftUI app.
- `CovalentIOS`: Tier 2 native SwiftUI app; supported independently so it cannot delay Tier 1 readiness.
- `CovalentShared`: versioned status models, local service client, and balanced security-scoped directory access.

Generate the Xcode project and build without signing:

```sh
cd apps/apple
xcodegen generate
xcodebuild -project Covalent.xcodeproj -scheme CovalentMac -destination 'platform=macOS' CODE_SIGNING_ALLOWED=NO build
xcodebuild -project Covalent.xcodeproj -scheme CovalentIOS -sdk iphonesimulator CODE_SIGNING_ALLOWED=NO build
```

The app selects folders through native pickers. It does not claim full-device iOS access. Background work is resumable and limited to APIs the platform grants.
