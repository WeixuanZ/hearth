# Contributing to hearth

Thank you for helping. Issues and pull requests are welcome, especially new
connectors and devices.

## Before your first pull request

hearth is offered under the GNU AGPL v3 and may also be offered under other
terms, such as a commercial licence. So that both stay possible, every
contributor signs the [Contributor License Agreement](CLA.md) once. You keep
the copyright in your work. When you open a pull request, the CLA Assistant
asks you to comment:

```text
I have read the CLA Document and I hereby sign the CLA
```

## Working on hearth

```sh
cargo test --workspace
cargo clippy --all-targets -- -D warnings
cargo fmt -p hearth
macos/build.sh            # the menu bar app, on a Mac
scripts/ci.sh             # everything CI runs, the Linux jobs in Docker
```

The [README](README.md#development) explains the architecture, and
[Adding a device](README.md#adding-a-device) walks through a new connector.
Keep to the style around you: small functions, plain names, comments that
say why.

## Your data stays yours

Tests, fixtures, docs and commit messages use made-up values: device names,
serials, addresses, codes. Never paste real ones from your home, logs or
config.
