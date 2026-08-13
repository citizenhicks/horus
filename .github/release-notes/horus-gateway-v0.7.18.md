# Horus Gateway 0.7.18

- Runs Full Access shell commands directly on the gateway host on macOS and Linux, allowing Xcode, App Store Connect, and other host-integrated commands to work without a nested Seatbelt or Bubblewrap wrapper.
- Keeps gateway state and TLS path protections active for restricted command modes.
- Continues scrubbing gateway and provider credential environment variables from shell processes.
- Preserves workspace working directories, command timeouts, output limits, and process cleanup.

Full Access commands can read gateway state, TLS credentials, stored provider credentials, and other files available to the gateway account.
