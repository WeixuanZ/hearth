# hearth

A local HomeKit bridge for devices that do not speak HomeKit.

One Rust process stays on the network and advertises a single bridge. Each device is a connector behind it. After a device is paired, hearth does not use that device's cloud. Home, scenes, and automations then see real accessories. Shortcuts and the manufacturer's app cannot do that.

An Apple TV or HomePod is the hub for control away from home. It does not replace this process. The machine running hearth has to stay awake, on the same network as the devices.

## Devices

Connectors included now. More can be added without a second bridge.

| Device | Connection | In Home |
|---|---|---|
| Dyson Purifier Hot+Cool Formaldehyde (HP09) | The fan's own MQTT, after one MyDyson login | One purifier: power, auto, 10 speeds, oscillation, heat to 1–37 °C, air quality, PM2.5, PM10, humidity, temperature, HEPA life, and switches for night, focused jet, keep sensing, and each oscillation sweep |
| Samsung The Frame, 2024 (LS03D, including the 65") | The TV's own websocket and IP Control, not SmartThings | One television: power, TV and HDMI 1–4 as inputs, remote, volume, art brightness. Art mode is a tile of its own |

Home has no formaldehyde sensor. Formaldehyde, VOC, and NO₂ are folded into the air-quality rating. The fan reports VOC and NO₂ as an index, not a density.

## Install

Once the repository is public:

```sh
curl -fsSL https://raw.githubusercontent.com/WeixuanZ/hearth/main/install.sh | sh
```

That downloads the latest release for this machine, checks it against `SHA256SUMS`, and puts `hearth` in `~/.local/bin`. It does not work while the repository is private: GitHub will not serve the script or the binaries without a login. To build from this checkout instead:

```sh
cargo install --path .
```

The computer and the devices have to be on the same network.

## Pair a device

A fan. `login` asks for the MyDyson password and the code Dyson emails. It stores the fan's local password, not the account password.

```sh
hearth dyson login --email you@example.com --country GB
```

A Frame. Turn the TV on, out of art mode. On the TV, allow device connection and Power On with Mobile (Settings → General → External Device Manager, and Connection), and turn on IP Remote (Settings → General → Network → Expert Settings).

```sh
hearth frame pair
hearth frame pair --host 192.168.1.40
```

Press Allow twice: once for the remote, once for IP Control. IP Control is what picks an input and powers the TV fully off. Without it, inputs fall back to remote keys, which newer firmware ignores. Both stay local.

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

**Rename a device.** Names come from the device or its app. To change one:

```sh
hearth rename "Old name" "New name"
```

then restart the bridge. Home picks the name up on its next refresh, or at once when the bridge is added again. Avoid naming a device after its room: Home hides a room's name at the start of an accessory's name.

**Add it again.** Remove the bridge in Home while it is running, and it shows up to add again. If it was removed while the bridge was stopped, the bridge still thinks it is paired and stays hidden. Then:

```sh
hearth unpair
```

restart the bridge, and add it as above.

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

**Heat.** Turning heat on turns the fan on, since it heats only while it blows. The fan stops heating when the focused jet is turned off, and Home shows the heater go off. Some firmware also ignores heat commands that did not come from the infrared remote. Set `heating = false` on that device to hide the control.

**Oscillation.** Home has no angle, so the purifier has a switch for each of the Dyson app's sweeps: 45, 90, 180 and 350 degrees, centred where the fan points. Home's own Oscillate keeps the range set in the Dyson app, or sweeps 90° if that range is narrower than 30°.

**Fan connection.** Set `host` on the fan if it does not announce itself on the network. The fan serves one live session, so stop the bridge before `hearth status` or `hearth dyson set`.

**The Frame.** Power is the TV's own on and off, and the inputs are TV and HDMI 1–4. Art mode is a tile of its own: Home shows a TV as power and an input picker only. Arrows, select, back and volume are in Control Center's remote, which keeps one connection to the TV open, so keys land at once. With IP Control, power and inputs go through it, and Home shows the input the TV is on. An input with nothing plugged in is refused. Without it, off holds the power key as the remote does, since a 2024 Frame ignores the power-off key, and on from standby is one press. Pairing and the art channel both need the panel awake.

To try a device without Home, the way Home drives it:

```sh
hearth dyson set swing 180
hearth frame power off
hearth frame art on
hearth frame input 2
hearth frame key KEY_MENU
```

## Adding a connector

A connector is a module under `src/` and a `kind` in the config. It borrows a block of HomeKit accessory ids (`aid`; the fan and the Frame use one each) and adds its accessories in `src/bridge.rs`. The bridge does not know how a device works.

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

A release is a tag that matches the version in `Cargo.toml`. For 0.2.0 the tag is `v0.2.0`. The build publishes Linux x86_64, Linux aarch64, macOS Apple silicon, and macOS Intel, plus the menu bar app and `SHA256SUMS`. The install command uses those files. Actions artifacts from the same run expire and are not the install source.

```sh
git tag v0.2.0
git push origin v0.2.0
```

## Libraries

`vendor/hap` is [ewilken/hap-rs](https://github.com/ewilken/hap-rs) `v0.1.0-pre.15` (MIT/Apache-2.0), with two changes. It looks up the machine's address through `if-addrs`, because the original pair of crates both try to link the same system library and no longer build. The television and input-source services include their linked service list, which that crate left out, so Home can show Art and the HDMI sockets as inputs.
