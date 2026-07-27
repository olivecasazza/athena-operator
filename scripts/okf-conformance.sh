#!/usr/bin/env bash
# Verify a rendered dossier against the REAL Open Knowledge Format validator.
#
# The operator enforces OKF's hard rules natively (dossier::okf_check) because
# it runs distroless with no package manager — but a native reimplementation is
# only ever as good as its author's reading of the spec. This script closes that
# gap by rendering a real dossier and running the reference implementation over
# it, so drift between the two is caught here rather than by a consumer.
#
# Verified equivalences (openknowledge 0.8.4, spec 0.1):
#   missing frontmatter  -> concept-frontmatter  (error, exit non-zero)
#   missing/empty type   -> concept-type         (error, exit non-zero)
#   broken link          -> link-target          (WARNING, exit zero)
# okf_check mirrors those names and, critically, also treats links as warnings —
# a gate stricter than the spec would refuse to publish conformant documents.
#
#   ./scripts/okf-conformance.sh
set -euo pipefail

cd "$(dirname "$0")/.."
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
bundle="$work/bundle"
mkdir -p "$bundle"

if ! command -v openknowledge >/dev/null 2>&1; then
  echo "installing openknowledge into $work/bin ..."
  curl -fsSL https://openknowledge.sh/install \
    | OPENKNOWLEDGE_INSTALL_DIR="$work/bin" bash >/dev/null
  export PATH="$work/bin:$PATH"
fi

echo "rendering a dossier from the real render path ..."
( cd operator && OKF_DUMP="$bundle/dossier.md" \
    cargo test -q -p athena-api --lib -- --ignored dump_dossier_for_external_validation ) >/dev/null

echo "validating with $(command -v openknowledge) ..."
if openknowledge validate --format json "$bundle" > "$work/report.json"; then
  python3 - "$work/report.json" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
s = r["summary"]
print(f"OKF {r.get('specVersion')} -> {s['status']}: "
      f"{s['errorCount']} error(s), {s['warningCount']} warning(s)")
for i in r.get("issues") or []:
    print(f"  {i.get('severity')} {i.get('rule')}: {i.get('message')}")
PY
  echo "PASS: the rendered dossier is conformant OKF."
else
  echo "FAIL: reference validator rejected the rendered dossier:" >&2
  cat "$work/report.json" >&2
  exit 1
fi
