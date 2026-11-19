#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCHEMATIC="$ROOT/tests/fixtures/minimal.kicad_sch"
REPORT="$(mktemp -t kicad-clippy-erc-XXXXXX.json)"
PROFILE="$(mktemp -t kicad-clippy-profile-XXXXXX.json)"
NETLIST_REPORT="$(mktemp -t kicad-clippy-netlist-XXXXXX.json)"
trap 'rm -f "$REPORT" "$PROFILE" "$NETLIST_REPORT"' EXIT

CLI="${KICAD_CLI:-}"
if [[ -z "$CLI" ]]; then
  CLI="$(command -v kicad-cli || command -v kicad-cli-nightly || true)"
fi
if [[ -z "$CLI" ]]; then
  echo "KiCad CLI not found. Set KICAD_CLI or install kicad-cli." >&2
  exit 1
fi

cd "$ROOT"
set +e
cargo run --locked --package kicad-clippy -- lint "$SCHEMATIC" --kicad-cli "$CLI" --format json > "$REPORT"
LINTER_STATUS=$?
set -e

if [[ ! -s "$REPORT" ]]; then
  echo "kicad-clippy did not write a JSON report (exit $LINTER_STATUS)." >&2
  exit 1
fi

jq -e '
  .syntax_valid == true
  and .erc_skipped == false
  and (.kicad_version | type == "string" and length > 0)
  and (.diagnostics | type == "array")
  and (.diagnostics | all(.[]; .code | startswith("KICAD/ERC/")))
  and (.summary.errors == ([.diagnostics[] | select(.severity == "error" and .excluded == false)] | length))
' "$REPORT" >/dev/null

ACTIVE_ERRORS="$(jq -r '.summary.errors' "$REPORT")"
EXPECTED_STATUS=0
if [[ "$ACTIVE_ERRORS" -gt 0 ]]; then
  EXPECTED_STATUS=1
fi
if [[ "$LINTER_STATUS" -ne "$EXPECTED_STATUS" ]]; then
  echo "Expected linter exit status $EXPECTED_STATUS for $ACTIVE_ERRORS active errors; got $LINTER_STATUS." >&2
  exit 1
fi

echo "Native ERC adapter passed: KiCad $(jq -r '.kicad_version' "$REPORT"), $ACTIVE_ERRORS active errors."

# Profile validation exports and parses KiCad's native flattened netlist,
# even when ERC itself is explicitly skipped.
printf '%s\n' '{"schema_version":1,"mode":"standard","components":{},"groups":{}}' > "$PROFILE"
cargo run --locked --package kicad-clippy -- lint "$SCHEMATIC" --kicad-cli "$CLI" --skip-erc --profile "$PROFILE" --format json > "$NETLIST_REPORT"
jq -e '
  .syntax_valid == true
  and .erc_skipped == true
  and (.diagnostics | type == "array")
  and (.summary.errors == ([.diagnostics[] | select(.severity == "error" and .excluded == false)] | length))
' "$NETLIST_REPORT" >/dev/null

echo "Native netlist export and pin-to-net parser passed."
