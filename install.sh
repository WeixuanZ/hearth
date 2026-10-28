#!/bin/sh
# Installs the hearth binary from the latest GitHub release.
#   curl -fsSL https://raw.githubusercontent.com/WeixuanZ/hearth/main/install.sh | sh
set -eu

repo="WeixuanZ/hearth"
version="${HEARTH_VERSION:-}"
dest="${HEARTH_BIN_DIR:-$HOME/.local/bin}"

os=$(uname -s)
machine=$(uname -m)
case "$os:$machine" in
  Linux:x86_64) target=linux-x86_64 ;;
  Linux:aarch64 | Linux:arm64) target=linux-aarch64 ;;
  Darwin:arm64) target=macos-arm64 ;;
  Darwin:x86_64) target=macos-x86_64 ;;
  *)
    echo "hearth: no build for $os $machine" >&2
    exit 1
    ;;
esac

if ! command -v curl >/dev/null 2>&1; then
  echo "hearth: curl is required" >&2
  exit 1
fi

if [ -z "$version" ]; then
  meta=$(curl -fsSL "https://api.github.com/repos/${repo}/releases/latest")
  version=$(printf '%s\n' "$meta" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
fi

case "$version" in
  v*) ;;
  "")
    echo "hearth: no release found. This fails while the repository is private." >&2
    exit 1
    ;;
  *) version="v${version}" ;;
esac

archive="hearth-${version}-${target}.tar.gz"
base="https://github.com/${repo}/releases/download/${version}"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "hearth: downloading ${version} for ${target}"
curl -fL --retry 3 -o "$tmp/$archive" "${base}/${archive}"
curl -fsSL --retry 3 -o "$tmp/SHA256SUMS" "${base}/SHA256SUMS"

expected=$(awk -v name="$archive" '
  {
    file = $2
    sub(/^\.\//, "", file)
    if (file == name) {
      print $1
      exit
    }
  }
' "$tmp/SHA256SUMS")
if [ -z "$expected" ]; then
  echo "hearth: ${archive} is missing from SHA256SUMS" >&2
  exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
  actual=$(sha256sum "$tmp/$archive" | awk '{print $1}')
else
  actual=$(shasum -a 256 "$tmp/$archive" | awk '{print $1}')
fi
if [ "$actual" != "$expected" ]; then
  echo "hearth: checksum did not match" >&2
  exit 1
fi

tar -xzf "$tmp/$archive" -C "$tmp"
binary="$tmp/hearth-${version}-${target}/hearth"
if [ ! -f "$binary" ]; then
  echo "hearth: the archive did not contain the binary" >&2
  exit 1
fi

mkdir -p "$dest"
cp "$binary" "$dest/hearth"
chmod 755 "$dest/hearth"
echo "hearth: installed $dest/hearth"

case ":$PATH:" in
  *":$dest:"*) ;;
  *) echo "hearth: add $dest to PATH" ;;
esac
