"""
Athena canary training entrypoint.
Exercises a real PyTorch training loop on a tiny GPT model with synthetic data.
Designed to be quick, deterministic, and runnable on both CPU and CUDA.

Environment variables:
    ATHENA_EXPERIMENT_SPEC      - CRD .spec JSON. Reads .parameters and .metrics.parser.path.
    ATHENA_EXPERIMENT_SPEC_PATH - path to a mounted CRD .spec JSON file.
    ATHENA_DEPTH                - override number of transformer layers (default: 2)
    ATHENA_DEVICE_BATCH_SIZE    - override micro-batch size (default: 4)
    ATHENA_TOTAL_BATCH_SIZE     - override total batch in tokens (default: 1024)
    ATHENA_TIME_BUDGET          - override max training seconds (default: 15)
    ATHENA_METRICS_PATH         - override metrics JSON path (default: metrics.json)
    ATHENA_DEVICE               - force device: "cpu" or "cuda" (default: auto-detect)
    ATHENA_SEQ_LEN              - override sequence length (default: 64)
    ATHENA_VOCAB_SIZE           - override vocabulary size (default: 256)

Outputs structured JSON to ATHENA_METRICS_PATH with:
    val_bpb, training_seconds, optimizer_steps, peak_vram_mb, status
"""

import json
import math
import os
import sys
import time
from contextlib import nullcontext

# Nix Python wrappers can leak an empty _PYTHON_SYSCONFIGDATA_NAME into uv
# subprocesses. PyTorch imports torch._dynamo during optimizer construction,
# which probes Triton; Triton calls sysconfig, and CPython then tries to import
# an empty sysconfigdata module. Drop only the broken empty value so the canary
# remains runnable under Nix and normal container Python alike.
if os.environ.get("_PYTHON_SYSCONFIGDATA_NAME") == "":
    os.environ.pop("_PYTHON_SYSCONFIGDATA_NAME")
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from opentelemetry import baggage, context, propagate, trace
from opentelemetry.exporter.otlp.proto.grpc.trace_exporter import OTLPSpanExporter
from opentelemetry.propagate import set_global_textmap
from opentelemetry.sdk.resources import SERVICE_NAME, SERVICE_VERSION, Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import BatchSpanProcessor
from opentelemetry.trace import SpanKind, get_current_span
from opentelemetry.trace.propagation.tracecontext import TraceContextTextMapPropagator

import torch
import torch.nn as nn
import torch.nn.functional as F


# ---------------------------------------------------------------------------
# Configuration from a transparent CRD spec, with legacy env overrides
# ---------------------------------------------------------------------------
@dataclass(frozen=True)
class CanaryConfig:
    depth: int = 2
    device_batch_size: int = 4
    total_batch_size: int = 1024
    time_budget: int = 15
    metrics_path: str = "metrics.json"
    seq_len: int = 64
    vocab_size: int = 256


def _load_json_object(raw: str, source: str) -> dict[str, Any]:
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as e:
        print(f"[canary] invalid JSON in {source}: {e}", file=sys.stderr)
        raise SystemExit(2) from e
    if not isinstance(value, dict):
        print(f"[canary] {source} must contain a JSON object", file=sys.stderr)
        raise SystemExit(2)
    return value


def load_crd_spec() -> dict[str, Any]:
    """Load a transparent Experiment/BenchmarkRun spec from env or file.

    The operator can inject the CRD's `.spec` verbatim as JSON via
    ATHENA_EXPERIMENT_SPEC, or mount it and point ATHENA_EXPERIMENT_SPEC_PATH at
    the file.  Keeping this shape close to the CRD avoids one-env-var-per-field
    glue in canary jobs while preserving old ATHENA_* overrides below.
    """
    raw = os.environ.get("ATHENA_EXPERIMENT_SPEC")
    if raw:
        return _load_json_object(raw, "ATHENA_EXPERIMENT_SPEC")

    spec_path = os.environ.get("ATHENA_EXPERIMENT_SPEC_PATH")
    if spec_path:
        try:
            return _load_json_object(Path(spec_path).read_text(), spec_path)
        except OSError as e:
            print(f"[canary] cannot read ATHENA_EXPERIMENT_SPEC_PATH={spec_path}: {e}", file=sys.stderr)
            raise SystemExit(2) from e

    return {}


def _nested_mapping(mapping: dict[str, Any], *path: str) -> dict[str, Any]:
    current: Any = mapping
    for key in path:
        if not isinstance(current, dict):
            return {}
        current = current.get(key, {})
    return current if isinstance(current, dict) else {}


def _get_int(mapping: dict[str, Any], key: str, default: int) -> int:
    value = mapping.get(key, default)
    try:
        return int(value)
    except (TypeError, ValueError) as e:
        print(f"[canary] config field {key!r} must be an integer, got {value!r}", file=sys.stderr)
        raise SystemExit(2) from e


def _env_int(name: str, current: int) -> int:
    value = os.environ.get(name)
    if value is None:
        return current
    try:
        return int(value)
    except ValueError as e:
        print(f"[canary] environment variable {name} must be an integer, got {value!r}", file=sys.stderr)
        raise SystemExit(2) from e


def load_config() -> CanaryConfig:
    spec = load_crd_spec()
    parameters = _nested_mapping(spec, "parameters") or spec
    metrics_parser = _nested_mapping(spec, "metrics", "parser")

    config = CanaryConfig(
        depth=_get_int(parameters, "depth", CanaryConfig.depth),
        device_batch_size=_get_int(parameters, "deviceBatchSize", CanaryConfig.device_batch_size),
        total_batch_size=_get_int(parameters, "totalBatchSize", CanaryConfig.total_batch_size),
        time_budget=_get_int(parameters, "timeBudget", CanaryConfig.time_budget),
        metrics_path=str(metrics_parser.get("path", CanaryConfig.metrics_path)),
        seq_len=_get_int(parameters, "seqLen", CanaryConfig.seq_len),
        vocab_size=_get_int(parameters, "vocabSize", CanaryConfig.vocab_size),
    )

    return CanaryConfig(
        depth=_env_int("ATHENA_DEPTH", config.depth),
        device_batch_size=_env_int("ATHENA_DEVICE_BATCH_SIZE", config.device_batch_size),
        total_batch_size=_env_int("ATHENA_TOTAL_BATCH_SIZE", config.total_batch_size),
        time_budget=_env_int("ATHENA_TIME_BUDGET", config.time_budget),
        metrics_path=os.environ.get("ATHENA_METRICS_PATH", config.metrics_path),
        seq_len=_env_int("ATHENA_SEQ_LEN", config.seq_len),
        vocab_size=_env_int("ATHENA_VOCAB_SIZE", config.vocab_size),
    )


def select_device():
    forced = os.environ.get("ATHENA_DEVICE")
    if forced:
        return torch.device(forced)
    if torch.cuda.is_available():
        return torch.device("cuda")
    return torch.device("cpu")


# ---------------------------------------------------------------------------
# OpenTelemetry observability
# ---------------------------------------------------------------------------
def init_observability() -> trace.Tracer:
    set_global_textmap(TraceContextTextMapPropagator())
    service_name = os.environ.get("OTEL_SERVICE_NAME", "athena-canary-experiment")
    provider = TracerProvider(
        resource=Resource.create(
            {
                SERVICE_NAME: service_name,
                SERVICE_VERSION: "0.1.0",
            }
        )
    )
    if os.environ.get("OTEL_EXPORTER_OTLP_ENDPOINT"):
        provider.add_span_processor(BatchSpanProcessor(OTLPSpanExporter()))
    trace.set_tracer_provider(provider)

    carrier = {}
    if os.environ.get("TRACEPARENT"):
        carrier["traceparent"] = os.environ["TRACEPARENT"]
    if os.environ.get("TRACESTATE"):
        carrier["tracestate"] = os.environ["TRACESTATE"]
    if carrier:
        context.attach(propagate.extract(carrier))

    for key in ("ATHENA_EXPERIMENT", "ATHENA_CAMPAIGN", "ATHENA_NAMESPACE"):
        if os.environ.get(key):
            context.attach(baggage.set_baggage(key.lower(), os.environ[key]))

    return trace.get_tracer(service_name)


def current_trace_metrics() -> dict[str, str]:
    span_context = get_current_span().get_span_context()
    if not span_context.is_valid:
        return {"trace_id": "", "span_id": ""}
    return {
        "trace_id": format(span_context.trace_id, "032x"),
        "span_id": format(span_context.span_id, "016x"),
    }


def shutdown_observability() -> None:
    provider = trace.get_tracer_provider()
    shutdown = getattr(provider, "shutdown", None)
    if callable(shutdown):
        shutdown()


# ---------------------------------------------------------------------------
# Tiny GPT model (simplified from train.py, no flash-attn dependency)
# ---------------------------------------------------------------------------
class TinyAttention(nn.Module):
    def __init__(self, n_embd, n_head):
        super().__init__()
        self.n_head = n_head
        self.head_dim = n_embd // n_head
        self.qkv = nn.Linear(n_embd, 3 * n_embd, bias=False)
        self.proj = nn.Linear(n_embd, n_embd, bias=False)

    def forward(self, x):
        B, T, C = x.size()
        qkv = self.qkv(x).view(B, T, 3, self.n_head, self.head_dim)
        qkv = qkv.permute(2, 0, 3, 1, 4)  # (3, B, nh, T, hd)
        q, k, v = qkv.unbind(0)
        y = F.scaled_dot_product_attention(q, k, v, is_causal=True)
        y = y.transpose(1, 2).contiguous().view(B, T, C)
        return self.proj(y)


class TinyMLP(nn.Module):
    def __init__(self, n_embd):
        super().__init__()
        self.fc = nn.Linear(n_embd, 4 * n_embd, bias=False)
        self.proj = nn.Linear(4 * n_embd, n_embd, bias=False)

    def forward(self, x):
        return self.proj(F.gelu(self.fc(x)))


class TinyBlock(nn.Module):
    def __init__(self, n_embd, n_head):
        super().__init__()
        self.ln1 = nn.LayerNorm(n_embd)
        self.attn = TinyAttention(n_embd, n_head)
        self.ln2 = nn.LayerNorm(n_embd)
        self.mlp = TinyMLP(n_embd)

    def forward(self, x):
        x = x + self.attn(self.ln1(x))
        x = x + self.mlp(self.ln2(x))
        return x


class TinyGPT(nn.Module):
    def __init__(self, vocab_size, n_embd, n_head, n_layer, seq_len):
        super().__init__()
        self.wte = nn.Embedding(vocab_size, n_embd)
        self.wpe = nn.Embedding(seq_len, n_embd)
        self.blocks = nn.ModuleList([TinyBlock(n_embd, n_head) for _ in range(n_layer)])
        self.ln_f = nn.LayerNorm(n_embd)
        self.lm_head = nn.Linear(n_embd, vocab_size, bias=False)
        self.seq_len = seq_len

    def forward(self, idx, targets=None):
        B, T = idx.size()
        pos = torch.arange(0, T, device=idx.device).unsqueeze(0)
        x = self.wte(idx) + self.wpe(pos)
        for block in self.blocks:
            x = block(x)
        x = self.ln_f(x)
        logits = self.lm_head(x)
        if targets is not None:
            loss = F.cross_entropy(logits.reshape(-1, logits.size(-1)), targets.reshape(-1))
            return loss
        return logits


# ---------------------------------------------------------------------------
# Deterministic synthetic data
# ---------------------------------------------------------------------------
def make_synthetic_data(num_batches, batch_size, seq_len, vocab_size, seed=42):
    """Generate deterministic token sequences with learnable patterns."""
    g = torch.Generator().manual_seed(seed)
    data = []
    for _ in range(num_batches):
        tokens = torch.randint(0, vocab_size, (batch_size, seq_len + 1), generator=g)
        x = tokens[:, :-1]
        y = tokens[:, 1:]
        data.append((x, y))
    return data


# ---------------------------------------------------------------------------
# Validation (computes bits-per-byte on synthetic held-out data)
# ---------------------------------------------------------------------------
@torch.no_grad()
def evaluate(model, val_data, device):
    model.eval()
    total_loss = 0.0
    total_tokens = 0
    for x, y in val_data:
        x, y = x.to(device), y.to(device)
        loss = model(x, y)
        total_loss += loss.item() * y.numel()
        total_tokens += y.numel()
    model.train()
    avg_nll = total_loss / total_tokens
    # Convert nats-per-token to bits-per-byte.
    # For synthetic uniform data with VOCAB_SIZE tokens, assume ~1 byte per token.
    bpb = avg_nll / math.log(2)
    return bpb


# ---------------------------------------------------------------------------
# Main training loop
# ---------------------------------------------------------------------------
def main():
    tracer = init_observability()
    config = load_config()
    device = select_device()
    use_cuda = device.type == "cuda"

    with tracer.start_as_current_span("athena.canary.train", kind=SpanKind.INTERNAL) as root_span:
        root_span.set_attribute("athena.depth", config.depth)
        root_span.set_attribute("athena.device_batch_size", config.device_batch_size)
        root_span.set_attribute("athena.total_batch_size", config.total_batch_size)
        root_span.set_attribute("athena.time_budget", config.time_budget)
        root_span.set_attribute("athena.seq_len", config.seq_len)
        root_span.set_attribute("athena.vocab_size", config.vocab_size)

        return _run_training(config, device, use_cuda, tracer)


def _run_training(config: CanaryConfig, device: torch.device, use_cuda: bool, tracer: trace.Tracer) -> int:
    print(
        f"[canary] device={device}, depth={config.depth}, "
        f"batch={config.device_batch_size}, total_batch={config.total_batch_size}, "
        f"time_budget={config.time_budget}s, seq_len={config.seq_len}, "
        f"vocab={config.vocab_size}"
    )

    torch.manual_seed(42)
    if use_cuda:
        torch.cuda.manual_seed(42)
        torch.cuda.reset_peak_memory_stats(device)

    # Model dimensions scale with depth (like train.py's ASPECT_RATIO pattern)
    n_embd = max(64, config.depth * 32)
    # Round to nearest multiple of n_head
    n_head = max(1, n_embd // 32)
    n_embd = n_head * 32  # ensure divisibility

    model = TinyGPT(config.vocab_size, n_embd, n_head, config.depth, config.seq_len).to(device)
    num_params = sum(p.numel() for p in model.parameters())
    print(f"[canary] model params: {num_params:,} (n_embd={n_embd}, n_head={n_head})")

    # Gradient accumulation
    tokens_per_micro = config.device_batch_size * config.seq_len
    grad_accum_steps = max(1, config.total_batch_size // tokens_per_micro)

    # Generate synthetic train/val data
    num_train_batches = max(grad_accum_steps * 20, 10)  # enough for a few optimizer steps
    num_val_batches = 4
    train_data = make_synthetic_data(num_train_batches, config.device_batch_size, config.seq_len, config.vocab_size, seed=42)
    val_data = make_synthetic_data(num_val_batches, config.device_batch_size, config.seq_len, config.vocab_size, seed=99)

    optimizer = torch.optim.AdamW(model.parameters(), lr=3e-4, weight_decay=0.01, foreach=False)

    # Initial val
    with tracer.start_as_current_span("athena.canary.evaluate.initial"):
        val_bpb_initial = evaluate(model, val_data, device)
    print(f"[canary] initial val_bpb={val_bpb_initial:.4f}")

    t_start = time.time()
    step = 0
    batch_idx = 0
    val_bpb = val_bpb_initial
    status = "completed"

    try:
        while True:
            elapsed = time.time() - t_start
            if elapsed >= config.time_budget:
                break

            span_context = (
                tracer.start_as_current_span("athena.canary.optimizer_step")
                if step == 0 or (step + 1) % 100 == 0
                else nullcontext()
            )
            with span_context as step_span:
                if step_span is not None:
                    step_span.set_attribute("athena.optimizer_step", step + 1)
                optimizer.zero_grad()
                accum_loss = 0.0
                for _acc in range(grad_accum_steps):
                    x, y = train_data[batch_idx % len(train_data)]
                    x, y = x.to(device), y.to(device)
                    loss = model(x, y) / grad_accum_steps
                    loss.backward()
                    accum_loss += loss.item()
                    batch_idx += 1

                torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
                optimizer.step()
                step += 1
                if step_span is not None:
                    step_span.set_attribute("athena.loss", accum_loss)

            if step % 10 == 0:
                print(f"[canary] step={step}, loss={accum_loss:.4f}, "
                      f"elapsed={time.time() - t_start:.1f}s")

        with tracer.start_as_current_span("athena.canary.evaluate.final"):
            val_bpb = evaluate(model, val_data, device)

    except Exception as e:
        status = f"error: {e}"
        print(f"[canary] ERROR: {e}", file=sys.stderr)

    training_seconds = time.time() - t_start

    peak_vram_mb = 0.0
    if use_cuda:
        peak_vram_mb = torch.cuda.max_memory_allocated(device) / (1024 * 1024)

    trace_metrics = current_trace_metrics()
    metrics = {
        "val_bpb": round(val_bpb, 6),
        "training_seconds": round(training_seconds, 3),
        "optimizer_steps": step,
        "peak_vram_mb": round(peak_vram_mb, 1),
        "status": status,
        "num_params": num_params,
        "device": str(device),
        "trace_id": trace_metrics["trace_id"],
        "span_id": trace_metrics["span_id"],
    }

    print(f"[canary] final: {json.dumps(metrics, indent=2)}")

    metrics_dir = os.path.dirname(config.metrics_path)
    if metrics_dir:
        os.makedirs(metrics_dir, exist_ok=True)
    with open(config.metrics_path, "w") as f:
        json.dump(metrics, f, indent=2)
    print(f"[canary] metrics written to {config.metrics_path}")

    shutdown_observability()
    return 0 if status == "completed" else 1


if __name__ == "__main__":
    sys.exit(main())
