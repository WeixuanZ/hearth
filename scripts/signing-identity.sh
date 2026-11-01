#!/bin/sh
# Makes "Hearth Local", a code-signing identity of this Mac's own, in the
# login keychain. macos/build.sh then signs with it, so a rebuilt Hearth.app
# keeps what macOS has allowed it, like the local network. Run it once; macOS
# asks for your password to trust the certificate for code signing.
set -eu
name="Hearth Local"
keychain="$HOME/Library/Keychains/login.keychain-db"

if security find-identity -v -p codesigning | grep -q "\"$name\""; then
  echo "$name already exists."
  exit 0
fi

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
cat > "$dir/cert.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $name
[ext]
basicConstraints = critical, CA:false
keyUsage = critical, digitalSignature
extendedKeyUsage = critical, codeSigning
EOF

# The system's openssl writes a bundle the keychain imports as it is.
/usr/bin/openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
  -config "$dir/cert.cnf" -keyout "$dir/key.pem" -out "$dir/cert.pem" 2>/dev/null
/usr/bin/openssl pkcs12 -export -name "$name" -passout pass:hearth \
  -inkey "$dir/key.pem" -in "$dir/cert.pem" -out "$dir/identity.p12"
security import "$dir/identity.p12" -k "$keychain" -P hearth -T /usr/bin/codesign
security add-trusted-cert -p codeSign -k "$keychain" "$dir/cert.pem"

echo "Made $name. macos/build.sh signs with it from now on."
echo "Rebuild once, then allow Hearth under Privacy & Security, Local Network."
