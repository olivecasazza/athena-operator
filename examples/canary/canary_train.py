"""
Athena canary training entrypoint.
Exercises a real PyTorch training loop on a tiny GPT model with synthetic data.
Designed to be quick, deterministic, and runnable on both CPU and CUDA.

Environment variables:
    ATHENA_DEPTH            - number of transformer layers (default: 2)
    ATHENA_DEVICE_BATCH_SIZE - micro-batch size (default: 4)
    ATHENA_TOTAL_BATCH_SIZE  - total batch in tokens (default: 1024)
    ATHENA_TIME_BUDGET       - max training seconds (default: 15)
    ATHENA_METRICS_PATH      - output path for metrics JSON (default: metrics.json)
    ATHENA_DEVICE            - force device: "cpu" or "cuda" (default: auto-detect)
    ATHENA_SEQ_LEN           - sequence length (default: 64)
    ATHENA_VOCAB_SIZE        - vocabulary size (default: 256)

Outputs structured JSON to ATHENA_METRICS_PATH with:
    val_bpb, training_seconds, optimizer_steps, peak_vram_mb, status
"""

import json
import math
import os
import time
import sys

import torch
import torch.nn as nn
import torch.nn.functional as F


# ---------------------------------------------------------------------------
# Configuration from env
# ---------------------------------------------------------------------------
DEPTH = int(os.environ.get("ATHENA_DEPTH", "2"))
DEVICE_BATCH_SIZE = int(os.environ.get("ATHENA_DEVICE_BATCH_SIZE", "4"))
TOTAL_BATCH_SIZE = int(os.environ.get("ATHENA_TOTAL_BATCH_SIZE", "1024"))
TIME_BUDGET = int(os.environ.get("ATHENA_TIME_BUDGET", "15"))
METRICS_PATH = os.environ.get("ATHENA_METRICS_PATH", "metrics.json")
SEQ_LEN = int(os.environ.get("ATHENA_SEQ_LEN", "64"))
VOCAB_SIZE = int(os.environ.get("ATHENA_VOCAB_SIZE", "256"))


def select_device():
    forced = os.environ.get("ATHENA_DEVICE")
    if forced:
        return torch.device(forced)
    if torch.cuda.is_available():
        return torch.device("cuda")
    return torch.device("cpu")


DEVICE = select_device()
USE_CUDA = DEVICE.type == "cuda"


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
    print(f"[canary] device={DEVICE}, depth={DEPTH}, batch={DEVICE_BATCH_SIZE}, "
          f"total_batch={TOTAL_BATCH_SIZE}, time_budget={TIME_BUDGET}s, "
          f"seq_len={SEQ_LEN}, vocab={VOCAB_SIZE}")

    torch.manual_seed(42)
    if USE_CUDA:
        torch.cuda.manual_seed(42)
        torch.cuda.reset_peak_memory_stats(DEVICE)

    # Model dimensions scale with depth (like train.py's ASPECT_RATIO pattern)
    n_embd = max(64, DEPTH * 32)
    # Round to nearest multiple of n_head
    n_head = max(1, n_embd // 32)
    n_embd = n_head * 32  # ensure divisibility

    model = TinyGPT(VOCAB_SIZE, n_embd, n_head, DEPTH, SEQ_LEN).to(DEVICE)
    num_params = sum(p.numel() for p in model.parameters())
    print(f"[canary] model params: {num_params:,} (n_embd={n_embd}, n_head={n_head})")

    # Gradient accumulation
    tokens_per_micro = DEVICE_BATCH_SIZE * SEQ_LEN
    grad_accum_steps = max(1, TOTAL_BATCH_SIZE // tokens_per_micro)

    # Generate synthetic train/val data
    num_train_batches = max(grad_accum_steps * 20, 10)  # enough for a few optimizer steps
    num_val_batches = 4
    train_data = make_synthetic_data(num_train_batches, DEVICE_BATCH_SIZE, SEQ_LEN, VOCAB_SIZE, seed=42)
    val_data = make_synthetic_data(num_val_batches, DEVICE_BATCH_SIZE, SEQ_LEN, VOCAB_SIZE, seed=99)

    optimizer = torch.optim.AdamW(model.parameters(), lr=3e-4, weight_decay=0.01)

    # Initial val
    val_bpb_initial = evaluate(model, val_data, DEVICE)
    print(f"[canary] initial val_bpb={val_bpb_initial:.4f}")

    t_start = time.time()
    step = 0
    batch_idx = 0
    val_bpb = val_bpb_initial
    status = "completed"

    try:
        while True:
            elapsed = time.time() - t_start
            if elapsed >= TIME_BUDGET:
                break

            optimizer.zero_grad()
            accum_loss = 0.0
            for _acc in range(grad_accum_steps):
                x, y = train_data[batch_idx % len(train_data)]
                x, y = x.to(DEVICE), y.to(DEVICE)
                loss = model(x, y) / grad_accum_steps
                loss.backward()
                accum_loss += loss.item()
                batch_idx += 1

            torch.nn.utils.clip_grad_norm_(model.parameters(), 1.0)
            optimizer.step()
            step += 1

            if step % 10 == 0:
                print(f"[canary] step={step}, loss={accum_loss:.4f}, "
                      f"elapsed={time.time() - t_start:.1f}s")

        val_bpb = evaluate(model, val_data, DEVICE)

    except Exception as e:
        status = f"error: {e}"
        print(f"[canary] ERROR: {e}", file=sys.stderr)

    training_seconds = time.time() - t_start

    peak_vram_mb = 0.0
    if USE_CUDA:
        peak_vram_mb = torch.cuda.max_memory_allocated(DEVICE) / (1024 * 1024)

    metrics = {
        "val_bpb": round(val_bpb, 6),
        "training_seconds": round(training_seconds, 3),
        "optimizer_steps": step,
        "peak_vram_mb": round(peak_vram_mb, 1),
        "status": status,
        "num_params": num_params,
        "device": str(DEVICE),
    }

    print(f"[canary] final: {json.dumps(metrics, indent=2)}")

    with open(METRICS_PATH, "w") as f:
        json.dump(metrics, f, indent=2)
    print(f"[canary] metrics written to {METRICS_PATH}")

    return 0 if status == "completed" else 1


if __name__ == "__main__":
    sys.exit(main())
