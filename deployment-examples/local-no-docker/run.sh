#!/usr/bin/env bash
# Start the CAS/AC server, the scheduler and one local worker.
# Press Ctrl-C to stop all three.
set -euo pipefail
cd "$(dirname "$0")"

BIN=${NATIVELINK_BIN:-}
if [ -z "$BIN" ]; then
  cargo build --release --bin nativelink --manifest-path ../../Cargo.toml
  BIN=../../target/release/nativelink
fi

trap 'kill 0' EXIT

"$BIN" cas.json5 &
"$BIN" scheduler.json5 &
# Give the scheduler time to listen. The worker retries anyway, but it logs
# an error on the first attempt.
sleep 2
"$BIN" worker.json5 &

wait
