# hearth

Puts devices that are not HomeKit into Apple Home.

One Rust process stays on the local network and speaks HomeKit. Each device is a connector behind that one bridge. After the first pairing of a device, hearth does not use that device's cloud.

Apple Home will only show something that stays on the LAN and speaks HomeKit. Shortcuts and the manufacturer's app cannot do this. They are not accessories, so they do not appear in the Home app, in scenes, or in automations.

## Devices

**Dyson Purifier Hot+Cool Formaldehyde** (HP09). The fan's own MQTT, after one MyDyson login.

In Home: the purifier (on/off, auto or manual, speed, oscillation), air quality, PM2.5, PM10, humidity, temperature, heat, HEPA filter life, and three switches: Night mode, Focused jet, Keep sensing.

Formaldehyde, VOC and NO₂ are folded into the air-quality rating. Home has no formaldehyde sensor, and the fan reports VOC and NO₂ as an index rather than a density.

**Samsung The Frame, 2024**, including the 65" (QE65LS03D and the other LS03D sizes). The TV's own websocket on port 8002, not SmartThings.

In Home: one television. On and off, the remote buttons, volume, art mode as an input, HDMI 1–4, and art brightness.

## Set up

The computer and the devices have to be on the same network. Install a Rust toolchain, then:

```sh
cargo install --path .
```

A fan:

```sh
hearth dyson login --email you@example.com --country GB
```

`login` asks for the MyDyson password and the code Dyson emails you. It saves the fan's local password. The account password is not stored.

A Frame. Turn the TV on first. On the TV, allow device connection and "Power On with Mobile" (Settings → General → External Device Manager, and Connection).

```sh
hearth frame pair
hearth frame pair --host 192.168.1.40
```

If the screen asks, press Allow. That is the only time the TV has to be on for pairing.

Then:

```sh
hearth status
hearth run
```

Config lives in `~/.config/hearth/config.toml` (mode 0600). An existing `~/.config/dyson-home` config is copied across on the first run, including the HomeKit pairing, so the fan does not have to be paired again. The bridge is now called Hearth. If Home still shows the old bridge name, remove it and add Hearth.

```sh
hearth pin
```

prints the 8-digit code. In the Home app: **Add Accessory → More options… → Hearth**.

An Apple TV or HomePod on the same network is what lets you use it away from home. This process still has to be running. The hub does not replace it.

## Keep it running

The computer must not be asleep, or Home shows the accessories as not responding. On a Mac, System Settings → Energy, or a launch agent:

```sh
# edit the two paths in the plist, then:
cp contrib/com.weixuan.hearth.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.weixuan.hearth.plist
```

## Heat

The fan's firmware sometimes ignores heat commands that did not come from the infrared remote. If Home's heat control snaps back to off, that is the fan refusing, not the bridge. Set `heating = false` on the fan's device in the config to hide it.

## The Frame

Art mode is a separate channel from the remote. On some 2024 firmware that channel never answers. Hearth then does what the Frame's own power button does: one press from a source enters art, one press in art leaves it. If Home's Art input and the screen disagree, that is the set, not a lost connection.

Off sends the TV's power-off key, which is standby, not art. On sends a wake packet to the MAC address learned while the TV was on, then the power key if it is still dark.

If the TV is fully off, `hearth status` cannot see it. Pairing and the art channel both need the panel awake.

## Adding a connector

A connector is a module under `src/` and a `kind` in the config. It borrows a block of HomeKit accessory ids (`aid` in the config; the fan uses four, the Frame uses one) and adds its accessories to the bridge in `src/bridge.rs`. The bridge itself does not know how a device works.

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

`bridge_id` is hashed into the HomeKit device id. Do not change it, or Home treats the bridge as a new accessory.

## Libraries

The HomeKit protocol implementation in `vendor/hap` is [ewilken/hap-rs](https://github.com/ewilken/hap-rs) `v0.1.0-pre.15` (MIT/Apache-2.0), with two changes. It looks up the machine's address through `if-addrs`, because the original pair of crates both try to link the same system library and no longer build. The television and input-source services include their linked service list, which that crate left out, so Home can show Art and the HDMI sockets as inputs.
