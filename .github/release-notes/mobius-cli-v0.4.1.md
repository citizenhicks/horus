## Highlights

- Initializes the first local gateway with loopback TCP and a Cloudflare Quick Tunnel, then starts
  it through the ordinary supervised serve path.
- Waits for the tunnel endpoint during first startup and removes incomplete gateway state and its
  provisioned local credential if startup fails.
- Works with `mobius-gateway connect` to pair through either the advertised public WSS endpoint or
  local TCP endpoint using one shared code.

## Compatibility

- This client requires the 0.4.1 gateway and continues to use the 0.4.0 möbius framework contract.

## Install or upgrade

```sh
mobius-gateway exit
cargo install --force --locked mobius-cli
mobius-gateway init
```
