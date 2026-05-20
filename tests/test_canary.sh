#!/usr/bin/env bash
# Smoke test for canary_train.py on CPU with tiny settings.
# Validates that training runs and metrics JSON is well-formed.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_DIR"

METRICS_FILE=$(mktemp /tmp/canary_metrics_XXXXXX.json)
trap "rm -f $METRICS_FILE" EXIT

echo "=== Running canary_train.py on CPU with tiny settings ==="
ATHENA_DEVICE=cpu \
ATHENA_DEPTH=1 \
ATHENA_DEVICE_BATCH_SIZE=2 \
ATHENA_TOTAL_BATCH_SIZE=128 \
ATHENA_TIME_BUDGET=5 \
ATHENA_SEQ_LEN=32 \
ATHENA_VOCAB_SIZE=64 \
ATHENA_METRICS_PATH="$METRICS_FILE" \
  uv run python3 examples/canary/canary_train.py

echo ""
echo "=== Validating metrics JSON ==="
if [ ! -f "$METRICS_FILE" ]; then
  echo "FAIL: metrics file not created at $METRICS_FILE"
  exit 1
fi

# Validate JSON and required keys
uv run python3 -c "
import json, sys
with open('$METRICS_FILE') as f:
    m = json.load(f)
required = ['val_bpb', 'training_seconds', 'optimizer_steps', 'peak_vram_mb', 'status']
missing = [k for k in required if k not in m]
if missing:
    print(f'FAIL: missing keys: {missing}')
    sys.exit(1)
if m['status'] != 'completed':
    print(f'FAIL: status={m[\"status\"]}')
    sys.exit(1)
if m['optimizer_steps'] < 1:
    print(f'FAIL: no optimizer steps completed')
    sys.exit(1)
if not isinstance(m['val_bpb'], (int, float)) or m['val_bpb'] <= 0:
    print(f'FAIL: invalid val_bpb={m[\"val_bpb\"]}')
    sys.exit(1)
print(f'OK: {m[\"optimizer_steps\"]} steps, val_bpb={m[\"val_bpb\"]:.4f}, '
      f'time={m[\"training_seconds\"]:.1f}s, vram={m[\"peak_vram_mb\"]}MB')
"

echo ""
echo "=== Canary smoke test PASSED ==="
