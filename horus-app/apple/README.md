# Horus for Apple platforms

One SwiftUI client target builds for macOS 26+ and iOS/iPadOS 26+. Both platforms use the same `AppModel`, `GatewayClient`, pairing flow, and versioned Horus gateway protocol.

Open `HorusApp.xcodeproj` and run the shared `HorusApp` scheme on a Mac, iPhone, or iPad destination. Command-line builds use:

```sh
xcodebuild -project HorusApp.xcodeproj -scheme HorusApp \
  -destination 'generic/platform=macOS' CODE_SIGNING_ALLOWED=NO build

xcodebuild -project HorusApp.xcodeproj -scheme HorusApp \
  -destination 'generic/platform=iOS Simulator' CODE_SIGNING_ALLOWED=NO build
```

On macOS, pair with `tcp://localhost:PORT` for a gateway on that Mac. For an
iPhone, iPad, or a Mac connecting to another host, initialize a stopped TLS
gateway on that host, then start its supervised connection flow:

```sh
horus-gateway init --listen 0.0.0.0:8741 \
  --tls-cert /absolute/path/fullchain.pem \
  --tls-key /absolute/path/private-key.pem
horus-gateway connect --endpoint tls://gateway.example:8741
```

Choose **Add gateway** and paste the displayed setup code, or scan its QR with
the iPhone/iPad Camera. The QR opens Horus with the `wss://` address and
one-time code prefilled; pairing still requires confirmation. The remote
hostname must be routable and covered by a publicly trusted TLS certificate.
Plaintext remote endpoints are rejected.

The one-time code is only the first pairing credential. A successful pairing
returns a per-pairing bearer token, which this app stores in device-only Keychain
storage and uses for later connections. Provider credentials are write-only and
are never stored by this app.
