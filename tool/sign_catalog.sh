#!/usr/bin/env bash
# Signs a catalog.json with the Ed25519 key whose public half is built into the
# plugin (rust/qazvoice-ffi/src/lib.rs CATALOG_KEY). Writes <file>.sig (hex).
#   tool/sign_catalog.sh catalog.json [key.pem]
set -euo pipefail
F=$1
KEY=${2:-$HOME/Desktop/eq/qazstt/signing/qazvoice-catalog-ed25519.pem}
openssl pkeyutl -sign -inkey "$KEY" -rawin -in "$F" | xxd -p -c 256 | tr -d '\n' > "$F.sig"
echo "signed $F → $F.sig"
