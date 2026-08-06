# Horus for Apple platforms

One SwiftUI client target builds for macOS 26+ and iOS/iPadOS 26+. Both platforms use the same `AppModel`, `GatewayClient`, pairing flow, and versioned Horus gateway protocol.

Open `HorusApp.xcodeproj` and run the shared `HorusApp` scheme on a Mac, iPhone, or iPad destination. Command-line builds use:

```sh
xcodebuild -project HorusApp.xcodeproj -scheme HorusApp \
  -destination 'generic/platform=macOS' CODE_SIGNING_ALLOWED=NO build

xcodebuild -project HorusApp.xcodeproj -scheme HorusApp \
  -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO build
```

On macOS, pair with the advertised loopback endpoint for a gateway on that Mac.
For an iPhone, iPad, or a Mac connecting to another host, select **Quick Connect**
during first-time setup, or request a fresh code from an initialized gateway:

```sh
horus-gateway init
# Later, while the gateway is running:
horus-gateway connect
```

Choose **Add gateway** and paste the displayed setup code, or scan its QR with
the iPhone/iPad Camera. The QR opens Horus with the public `wss://` address and
one-time code prefilled; pairing still requires confirmation. The same one-use
code works through the advertised local `tcp://` endpoint. Plaintext remote
endpoints are rejected; a direct TLS listener remains available as an advanced
option in the gateway guide.

The one-time code is only the first pairing credential. A successful pairing
returns a per-pairing bearer token, which this app stores in device-only Keychain
storage and uses for later connections. Provider credentials are write-only and
are never stored by this app.
