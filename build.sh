#!/usr/bin/env bash
# Builds the wallet engine into web/pkg/: the crate for wasm32-unknown-unknown in the
# size-optimized `browser-release` profile, then `wasm-bindgen --target web`, which writes
# the `.wasm` and the JavaScript glue that loads it (an ES module, no bundler needed).
#
#   ./build.sh
#
# Needs the wasm32 target (rust-toolchain.toml installs it) and the wasm-bindgen CLI at
# exactly the version Cargo.lock pins:
#
#   cargo install wasm-bindgen-cli --version 0.2.130 --locked

set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

profile=browser-release
crate=paykit_wasm_wallet_demo
out=web/pkg

want=$(grep -A1 '^name = "wasm-bindgen"$' Cargo.lock | sed -n 's/^version = "\(.*\)"$/\1/p')
have=$(wasm-bindgen --version 2>/dev/null | awk '{print $2}') || true
if [[ "$have" != "$want" ]]; then
  echo "error: need wasm-bindgen $want on PATH, found '${have:-none}'" >&2
  echo "       cargo install wasm-bindgen-cli --version $want --locked" >&2
  exit 1
fi

echo "== cargo build ($profile, wasm32-unknown-unknown)"
cargo build --locked --target wasm32-unknown-unknown --profile "$profile"

echo "== wasm-bindgen --target web -> $out/"
rm -rf "$out"
wasm-bindgen --target web --no-typescript --out-dir "$out" \
  "${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/$profile/$crate.wasm"

echo "== sizes (bytes)"
for file in "$out/${crate}_bg.wasm" "$out/$crate.js"; do
  raw=$(wc -c <"$file" | tr -d ' ')
  gz=$(gzip -9 -c "$file" | wc -c | tr -d ' ')
  printf '   %-34s raw %10s   gzip -9 %10s\n' "$(basename "$file")" "$raw" "$gz"
done
