# Horus for Apple platforms

One SwiftUI client target builds for macOS 26+ and iOS/iPadOS 26+. Both platforms use the same `AppModel`, `GatewayClient`, pairing flow, and versioned Horus gateway protocol.

Open `HorusApp.xcodeproj` and run the shared `HorusApp` scheme on a Mac, iPhone, or iPad destination. Command-line builds use:

```sh
xcodebuild -project HorusApp.xcodeproj -scheme HorusApp \
  -destination 'generic/platform=macOS' CODE_SIGNING_ALLOWED=NO build

xcodebuild -project HorusApp.xcodeproj -scheme HorusApp \
  -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO build
```

On macOS, pair with `tcp://localhost:PORT` for a gateway on that Mac. An
iPhone, iPad, or a Mac connecting to another host uses `tls://HOST:PORT`; the
pairing screen and protocol are otherwise identical. Plaintext remote endpoints
are rejected. Gateway bearer tokens are stored in Keychain; provider
credentials are write-only and are never stored by this app.
