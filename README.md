# dyson-home

Puts a **Dyson Purifier Hot+Cool Formaldehyde** (HP09) into Apple Home.

The fan is not a HomeKit or Matter device. Apple Home will only show something
that stays on the local network and speaks HomeKit, so a bridge has to be
running. This is that bridge: one Rust program on the Mac, talking to the fan
over the fan's own MQTT. After the first login it does not use the Dyson cloud.

Shortcuts and the MyDyson app cannot do this. They are not accessories, so they
do not appear in the Home app, in scenes, or in automations.

## What shows up in Home

One bridge, paired once:

- the purifier: on/off, auto or manual, speed, oscillation
- air quality, PM2.5 and PM10
- humidity, and temperature
- heat, if the fan accepts it (see below)
- HEPA filter life
- three switches: **Night mode**, **Focused jet**, **Keep sensing**

Formaldehyde, VOC and NO₂ are folded into the air-quality rating. Home has no
formaldehyde sensor, and the fan reports VOC and NO₂ as an index rather than a
density, so showing those numbers as µg/m³ would be a lie.

## Set up

The Mac and the fan have to be on the same Wi-Fi. Install a Rust toolchain, then:

```sh
cargo install --path .
dyson-home login --email you@example.com --country GB
dyson-home status
dyson-home run
```

`login` asks for the MyDyson password and the code Dyson emails you. It saves
the fan's local password to `~/.config/dyson-home/config.toml` (mode 0600) and
prints an 8-digit pairing code. The account password is not stored.

In the Home app: **Add Accessory → More options… → the fan's name**, and enter
the code from `dyson-home pin`. An Apple TV or HomePod on the same network is
what lets you use it away from home. This process still has to be running; the
hub does not replace it.

If the fan is not discovered, set its address:

```toml
host = "192.168.1.50"
```

`product_type` is filled in by `login` (usually `527K` or `527E`). Leave it as
`auto` only if you are writing the config by hand.

## Keep it running

The Mac must not be asleep, or Home shows the accessory as not responding.
System Settings → Energy, or a launch agent:

```sh
# edit the two paths in the plist, then:
cp contrib/com.weixuan.dyson-home.plist ~/Library/LaunchAgents/
launchctl load ~/Library/LaunchAgents/com.weixuan.dyson-home.plist
```

## Heat

The fan's firmware sometimes ignores heat commands that did not come from the
infrared remote. If Home's heat control snaps back to off, that is the fan
refusing, not the bridge. Set `heating = false` in the config to hide it.
Fan, oscillation, night mode, auto and the sensors are not affected.

## Check it without Home

The HomeKit protocol implementation in `vendor/hap` is [ewilken/hap-rs](https://github.com/ewilken/hap-rs) `v0.1.0-pre.15` (MIT/Apache-2.0), with one change: it looks up the Mac's address through `if-addrs`, because the original pair of crates both try to link the same system library and no longer build.
