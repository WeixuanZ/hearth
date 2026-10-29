# hearth

A local, extensible bridge that puts devices without HomeKit into Apple Home, and gives them a menu bar, a CLI and an API for scripts and agents.

hearth is generic. Every device describes itself as a few **capabilities**: power, switches, choices, levels, readings and keys. Apple Home, the menu bar, the CLI and the control socket all work from those, so they work for any device, and adding a new one means teaching hearth a protocol, not building new UI. Devices are reached directly on your network, never through the maker's cloud once set up.

It runs as one process on a machine that stays awake on the same network as the devices. An Apple TV or HomePod still acts as the Home hub for control away from home.

## Supported devices

Two connectors so far, each with one device:

| Connector | Device | Reached over | Capabilities |
|---|---|---|---|
| Dyson | Purifier Hot+Cool Formaldehyde (HP09) | The fan's own MQTT, after one MyDyson login | power, auto, speed, oscillation, sweep, night mode, jet focus, continuous monitoring, heat, heat target, temperature, humidity, PM2.5, PM10, formaldehyde, filter life |
| Samsung | The Frame, 2024 (LS03D) | The TV's websocket and IP Control, not SmartThings | power, art mode, input (TV, HDMI 1–4), art brightness, remote keys and volume |

In Apple Home these become a purifier, and a television with an Art mode tile. More connectors can be added without touching the rest; see [Adding a device](#adding-a-device).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/WeixuanZ/hearth/main/install.sh | sh
```

downloads the latest release for this machine (Linux, or macOS on Apple silicon or Intel), checks it against `SHA256SUMS`, and puts `hearth` in `~/.local/bin`. Or build from a checkout with `cargo install --path .`.

## Set up

**A Dyson fan.** `login` asks for your MyDyson password and the code Dyson emails, then keeps only the fan's local password. It is the one time hearth talks to Dyson.

```sh
hearth dyson login --email you@example.com --country GB
```

**A Samsung Frame.** Turn the TV on, out of art mode. On the TV, turn on device connection and Power On with Mobile (Settings → General → External Device Manager, and Connection), and IP Remote (Settings → General → Network → Expert Settings). Then press Allow twice: once for the remote, once for IP Control, which picks inputs and powers the TV fully off.

```sh
hearth frame pair
```

**Apple Home.** Start the bridge, then in the Home app choose Add Accessory → More options → Hearth, and enter the code from `hearth pin` or the menu bar.

```sh
hearth run
```

## Use it

**Menu bar** (Mac). Shows the bridge, the pairing code while it is not in Home, and each device. Click a device to open its controls, drawn from its capabilities: toggles, choices, sliders, a remote, and readings. Build it with `macos/build.sh`, then `open macos/Hearth.app`.

**CLI.** Talks to the running bridge, so it shares the devices' connections and answers in milliseconds.

```sh
hearth devices                    # every device, each capability, what it accepts
hearth get tv                     # one device
hearth get purifier temperature
hearth set tv power on
hearth set tv input hdmi2
hearth set tv remote select
hearth set purifier speed 4
hearth set purifier sweep 90
```

A device is its id, name, kind (`tv`, `purifier`), connector (`frame`, `dyson`), or a part of its name. A value is `on`/`off`, a number, or an option id from `hearth devices`.

**Scripts and agents.** Add `--json` to any command. `hearth devices --json` lists every capability with its kind, value, accepted options or range, and whether it is active, which is enough to drive any device without knowing its brand. Errors go to stderr with exit status 1. Programs can also talk to the [control socket](#control-socket) directly.

**Rename a device** with `hearth rename "Old name" "New name"`, then restart the bridge. Names given in the Home app stick too.

**Start over with Home.** If Home will not add the bridge, or says it already belongs to a home, choose Forget Home in the menu bar (or stop the bridge and run `hearth unpair`). The bridge comes back as a new accessory. Remove any old Hearth from Home.

## Keep it running

Either the menu bar's Start, which logs to `~/.config/hearth/hearth.log`, or a launch agent that starts hearth at login (edit the paths first):

```sh
cp contrib/com.weixuan.hearth.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.weixuan.hearth.plist
```

Not both. Keep the machine from sleeping, or Home shows the devices as not responding.

## Good to know

- **Dyson heat.** Heat turns the fan on, since it heats only while it blows. The fan stops heating by itself when jet focus is turned off.
- **Dyson oscillation.** Home has no angle, so the purifier has a switch for each sweep the Dyson app offers.
- **Dyson sensors.** The fan measures while it runs, or while it is off with continuous monitoring on. Otherwise its sensors show as inactive.
- **Dyson address.** Set `host` on the fan in the config if it does not announce itself on the network.
- **Frame art mode** is a tile of its own, because Home shows a TV only as power and an input picker. An input with nothing plugged in is refused. Without IP Control, inputs fall back to remote keys, which newer firmware ignores.

## Development

### Architecture

```text
              surfaces: Apple Home · control socket (menu bar, scripts) · CLI
                                        │
                          capabilities: snapshots and requests
                                        │
              devices: one task per device, its state, its HomeKit accessories
                                        │
                 connectors: each maker's protocol, discovery and setup
```

```text
src/
  capabilities.rs        the vocabulary, device snapshots, and the registry
  connectors/dyson/      MyDyson login, MQTT, the fan's messages
  connectors/samsung/    pairing, the remote and art channels, IP Control
  devices/purifier/      task, HomeKit accessory, capabilities
  devices/tv/            task, HomeKit accessories, capabilities
  surfaces/homekit/      the HomeKit bridge and accessory helpers
  surfaces/control.rs    the control socket
  surfaces/cli.rs        what the CLI prints
  config.rs              ~/.config/hearth/config.toml
  run.rs                 hearth run: starts the devices and the surfaces
macos/                   the menu bar app (SwiftUI)
vendor/hap/              the HomeKit protocol, patched
```

- **Connectors** know how to talk to a maker: login, pairing, discovery and message formats. They know nothing about Home or the menu bar.
- **Devices** know what a product does. Each runs as one task that owns its state and its connection, turns requests into the connector's commands, and turns its state into a capability snapshot and HomeKit accessories.
- **Capabilities** are the shared vocabulary. A capability has an id, a name, a kind (`power`, `switch`, `choice`, `level`, `reading` or `keys`) with its options or range, a value, and whether it is active.
- **Surfaces** never know a brand. HomeKit, the socket and the CLI send requests to the same device task, so they always agree, and a device answers once it has acted so a refusal reaches whoever asked. The menu bar draws controls from capability kinds alone.

### Control socket

`~/.config/hearth/control.sock`, mode 0600, never on the network. One JSON request per line, one reply per line, served from memory in about ten milliseconds:

```text
{"op":"status"}                                                   → the bridge and every device
{"op":"get","device":"tv"}                                        → one device
{"op":"set","device":"tv","capability":"input","value":"hdmi2"}  → {"ok":true} or {"error":"…"}
```

### Adding a device

1. **Connector.** Under `src/connectors/<maker>/`: the protocol, and a `hearth <maker> …` setup command if the device needs pairing. Add its `kind` to `src/config.rs`.
2. **Device.** Under `src/devices/<type>/`: a task that keeps the device's state and connection (see `devices::tv::spawn`).
3. **Capabilities.** In `capabilities.rs` next to it: a `snapshot` from state, and a function from a request to the device's command. Publish with `capabilities::publish`; take requests from `capabilities::listen`.
4. **HomeKit.** In `accessory.rs`: the accessories, with fixed iids per service (tests check they are unique), `configured_name` for names Home can change, and the same commands as the capabilities.
5. **Run it.** Spawn it in `src/run.rs`.

The menu bar, the CLI and the socket need no changes.

### HomeKit notes

- A service linked to a TV shows up in its input picker, so the TV links only inputs.
- Home caches the accessory layout until the configuration number changes. hearth fingerprints the layout and bumps the number when it differs.
- `bridge_id` in the config is hashed into the HomeKit identity. Changing it makes Home see a new accessory, which is how `hearth unpair` starts over.

### Build, test and release

```sh
cargo test
cargo clippy --all-targets
macos/build.sh
```

Every push and pull request is checked on Linux and macOS: formatting, Clippy, tests, and the menu bar build. A tag that matches the version in `Cargo.toml`, like `v0.2.0`, builds and publishes Linux (x86_64, aarch64), macOS Apple silicon and macOS Intel binaries, the menu bar app, and `SHA256SUMS`.

### Libraries

`vendor/hap` is [ewilken/hap-rs](https://github.com/ewilken/hap-rs) `v0.1.0-pre.15` (MIT/Apache-2.0), patched as listed in `vendor/PATCH.txt`.

## License

Copyright (c) 2026 Weixuan Zhang. Licensed under the [GNU Affero General Public License v3.0 only](LICENSE).

You may use, change and share hearth. If you distribute it, or run a changed version as a network service, you must publish your complete source under the same license. For other terms, such as using hearth in a closed product, ask the author about a commercial license.
