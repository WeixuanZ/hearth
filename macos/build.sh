#!/bin/sh
# Builds the optional menu bar app. Run this on a Mac.
# The bridge itself does not need it.
set -eu
cd "$(dirname "$0")"
root=$(cd .. && pwd)
app="$root/macos/Hearth.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp Info.plist "$app/Contents/Info.plist"
cp icon.icns "$app/Contents/Resources/AppIcon.icns"

arch=$(uname -m)
swiftc -O -parse-as-library \
  -target "${arch}-apple-macosx13.0" \
  -framework SwiftUI -framework AppKit -framework ServiceManagement \
  -o "$app/Contents/MacOS/Hearth" \
  HearthBar.swift Model.swift Panel.swift MenuExtras.swift

# Not "hearth": on a case-insensitive disk that is the same file as "Hearth",
# and copying it would replace the app itself.
if [ -x "$root/target/release/hearth" ]; then
  cp "$root/target/release/hearth" "$app/Contents/MacOS/hearth-cli"
elif command -v hearth >/dev/null 2>&1; then
  cp "$(command -v hearth)" "$app/Contents/MacOS/hearth-cli"
else
  echo "No hearth binary bundled. The app will use ~/.cargo/bin/hearth if it is installed."
fi

# Apple Silicon will not run unsigned code, so sign ad hoc.
codesign --force --deep -s - "$app"

echo "Built $app"
echo "Open it, or copy it to /Applications. It has no Dock icon."
