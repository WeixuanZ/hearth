# hearth

A local HomeKit bridge for devices that do not speak HomeKit.

One Rust process stays on the network and advertises a single bridge. Each device is a connector behind it. After a device is paired, hearth does not use that device's cloud. Home, scenes, and automations then see real accessories. Shortcuts and the manufacturer's app cannot do that.

An Apple TV or HomePod is the hub for control away from home. It does not replace this process. The machine running hearth has to stay awake, on the same network as the devices.

## Devices

| Device | Connection | In Home |
|---|---|---|
| Dyson Purifier Hot+Cool Formaldehyde (HP09) | The fan's own MQTT, after one MyDyson login | Purifier, air quality, PM2.5, PM10, humidity, temperature, heat, HEPA life, and switches for night, focused jet, and keep sensing |
| Samsung The Frame, 2024 (LS03D, including the 65") | The TV's own websocket on port 8002, not SmartThings | One television: power, remote, volume, Art and HDMI 1–4 as inputs, art brightness |

Home has no formaldehyde sensor. Formaldehyde, VOC, and NO₂ are folded into the air-quality rating. The fan reports VOC and NO₂ as an index, not a density.

## Install

The computer and the devices have to be on the same network. Install a Rust toolchain, then:

```sh
cargo install --path .
```

Tagged releases also publish binaries. See [Releases](#releases).

## Pair a device

A fan. `login` asks for the MyDyson password and the code Dyson emails. It stores the fan's local password, not the account password.

```sh
hearth dyson login --email you@example.com --country GB
```

A Frame. Turn the TV on. On the TV, allow device connection and Power On with Mobile (Settings → General → External Device Manager, and Connection).

```sh
hearth frame pair
hearth frame pair --host 192.168.1.40
```

If the screen asks, press Allow. That is the only time the TV has to be on for pairing.

Check, then start:

```sh
hearth status
hearth run
```

## Add it to Home

```sh
hearth pin
```

prints the 8-digit code. In the Home app: **Add Accessory → More options… → Hearth**.

Config is `~/.config/hearth/config.toml`, mode 0600. An existing `~/.config/dyson-home` is copied on the first run, including the HomeKit pairing. If Home still shows the old bridge name, remove it and add Hearth.

`bridge_id` in that file is hashed into the HomeKit device id. Leave it alone, or Home treats the bridge as a new accessory.

## Keep it running

Use one of these. Not both: the launch agent restarts the bridge as soon as the menu bar stops it.

**Menu bar, Mac only.** It reads a status file the bridge already writes. It does not open a second connection to the fan or the TV. The panel shows whether the bridge is running, each device, and the pairing code. Start and Stop control the bridge. Quit closes the icon and leaves a running bridge alone. There is no Dock icon. To open it at login, add the app in System Settings → General → Login Items.

```sh
cargo build --release
macos/build.sh
open macos/Hearth.app
```

**Launch agent.** The machine must not sleep, or Home shows the accessories as not responding. Edit the two paths in the plist, then:

```sh
cp contrib/com.weixuan.hearth.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.weixuan.hearth.plist
```

System Settings → Energy is the other way to keep a Mac awake.

## Known limits

**Heat.** Some fan firmware ignores heat commands that did not come from the infrared remote. If Home's heat control snaps back to off, the fan refused it. Set `heating = false` on that device to hide the control.

**Art mode.** It is a separate channel from the remote. On some 2024 firmware that channel never answers. Hearth then does what the Frame's own power button does: one press from a source enters art, one press in art leaves it. If Home's Art input and the screen disagree, that is the set.

Off sends the TV's power-off key, which is standby, not art. On sends a wake packet to the MAC address learned while the TV was on, then the power key if it is still dark. If the TV is fully off, `hearth status` cannot see it. Pairing and the art channel both need the panel awake.

## Adding a connector

A connector is a module under `src/` and a `kind` in the config. It borrows a block of HomeKit accessory ids (`aid`; the fan uses four, the Frame uses one) and adds its accessories in `src/bridge.rs`. The bridge does not know how a device works.

```toml
[[devices]]
kind = "frame"
aid = 6
name = "The Frame"
model = "QE65LS03D"
host = "192.168.1.40"
mac = "aa:bb:cc:dd:ee:ff"
token = "…"
```

## Releases

Pushes to `main` and pull requests are formatted, linted, and tested. The menu bar is compiled on macOS as well.

A release is a tag that matches the version in `Cargo.toml`. For 0.2.0 the tag is `v0.2.0`. The build publishes Linux x86_64, Linux aarch64, macOS Apple silicon, and macOS Intel, plus the menu bar app and `SHA256SUMS`.

```sh
git tag v0.2.0
git push origin v0.2.0
```

## Libraries

`vendor/hap` is [ewilken/hap-rs](https://github.com/ewilken/hap-rs) `v0.1.0-pre.15` (MIT/Apache-2.0), with two changes. It looks up the machine's address through `if-addrs`, because the original pair of crates both try to link the same system library and no longer build. The television and input-source services include their linked service list, which that crate left out, so Home can show Art and the HDMI sockets as inputs.
