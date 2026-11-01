#!/bin/sh
# Builds the menu bar app, macos/Hearth.app. Run it on a Mac.
#   APP         where to build it. Defaults to macos/Hearth.app.
#   ARCH        arm64 or x86_64. Defaults to this Mac's.
#   HEARTH_BIN  the hearth binary to bundle. Defaults to target/release/hearth,
#               then the hearth on PATH.
set -eu
cd "$(dirname "$0")"
root=$(cd .. && pwd)
app="${APP:-$root/macos/Hearth.app}"
arch="${ARCH:-$(uname -m)}"

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp Info.plist "$app/Contents/Info.plist"
cp icon.icns "$app/Contents/Resources/AppIcon.icns"
# The app's version is hearth's, so the updater can compare it with releases.
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
plutil -replace CFBundleShortVersionString -string "$version" "$app/Contents/Info.plist"

swiftc -O -parse-as-library \
  -target "${arch}-apple-macosx13.0" \
  -framework SwiftUI -framework AppKit -framework Charts \
  -o "$app/Contents/MacOS/Hearth" \
  ./*.swift

# Not "hearth": on a case-insensitive disk that is the same file as "Hearth",
# and copying it would replace the app itself.
bin="${HEARTH_BIN:-$root/target/release/hearth}"
if [ ! -x "$bin" ]; then
  bin=$(command -v hearth || true)
fi
if [ -n "$bin" ]; then
  cp "$bin" "$app/Contents/MacOS/hearth-cli"
else
  echo "No hearth binary bundled. The app will use ~/.cargo/bin/hearth if it is installed."
fi

# Apple silicon will not run unsigned code, so sign ad hoc.
codesign --force --deep -s - "$app"

echo "Built $app ($arch)"
