"""DDPO aesthetic-alignment trainer for Athena campaigns.

Fine-tunes Stable Diffusion 1.5 with Denoising Diffusion Policy Optimization
(TRL's DDPOTrainer) against the improved-aesthetic-predictor reward, following
https://huggingface.co/blog/trl-ddpo. LoRA-only (full-model DDPO needs an A100;
the campaign targets the 8GB Quadro-RTX-4000 pool, so trainable params are the
LoRA adapter and nothing else).

Athena contract (all env-driven; unset = local no-op, mirroring
experiments/train.py):

  ATHENA_EXPERIMENT_SPEC   Experiment spec JSON; swept hyperparameters live in
                           .parameters (train_learning_rate, sample_num_steps,
                           sample_num_batches_per_epoch,
                           per_prompt_stat_tracking_buffer_size, num_epochs).
                           Batch sizes are deliberately NOT swept (they are
                           memory/hardware knobs on 8GB cards) — they come from
                           DDPO_* env vars in the RuntimeProfile.
  ATHENA_SEED              Per-generation seed from the campaign (common random
                           numbers: every experiment in a generation shares it,
                           so candidate-minus-control is a paired difference).
  ATHENA_RESUME_FROM       Directory of LoRA weights from the parent experiment
                           (PBT exploit = warm start). WEIGHTS ONLY: a child
                           runs new hyperparameters, so the optimizer state is
                           deliberately fresh — same rule as train.py.
  ATHENA_CHECKPOINT_DIR    Where final LoRA weights land for children to resume.
  ATHENA_WORKSPACE_PATH    Run workspace; metrics.json + sample grids go here.
  ATHENA_METRICS_PATH      (unused; metrics.json is written to the workspace)
  /dev/termination-log     Final JSON summary — the operator parses this.

Objective honesty (the v85 lesson, applied): the TRAINING reward mean is
hackable by construction — DDPO optimizes it directly. The campaign objective
is therefore the aesthetic score on HELD-OUT prompts the trainer never samples
during training (aesthetic_heldout_mean), with the training reward reported as
a diagnostic only.
"""

from __future__ import annotations

import json
import os
import random
import sys
import time

import numpy as np
import torch

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
try:
    from athena_metrics import AthenaMetrics, journal
except ImportError:  # local run without the experiments/ package on path
    sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    from athena_metrics import AthenaMetrics, journal

from huggingface_hub import hf_hub_download
from huggingface_hub.utils import EntryNotFoundError
from transformers import CLIPModel, CLIPProcessor

from trl import DDPOConfig, DDPOTrainer, DefaultDDPOStableDiffusionPipeline

# ── Reward model ─────────────────────────────────────────────────────────────
# improved-aesthetic-predictor: frozen CLIP ViT-L/14 image embedder + a small
# MLP regression head trained on AVA human-preference ratings. Public weights.
AESTHETIC_MODEL_ID = "trl-lib/ddpo-aesthetic-predictor"
AESTHETIC_MODEL_FILE = "aesthetic-model.pth"


class MLP(torch.nn.Module):
    def __init__(self):
        super().__init__()
        self.layers = torch.nn.Sequential(
            torch.nn.Linear(768, 1024),
            torch.nn.Dropout(0.2),
            torch.nn.Linear(1024, 128),
            torch.nn.Dropout(0.2),
            torch.nn.Linear(128, 64),
            torch.nn.Dropout(0.1),
            torch.nn.Linear(64, 16),
            torch.nn.Linear(16, 1),
        )

    def forward(self, embed):
        return self.layers(embed)


class AestheticScorer(torch.nn.Module):
    def __init__(self, *, dtype):
        super().__init__()
        self.clip = CLIPModel.from_pretrained("openai/clip-vit-large-patch14")
        self.processor = CLIPProcessor.from_pretrained("openai/clip-vit-large-patch14")
        self.mlp = MLP()
        try:
            cached = hf_hub_download(AESTHETIC_MODEL_ID, AESTHETIC_MODEL_FILE)
        except EntryNotFoundError:
            cached = os.path.join(AESTHETIC_MODEL_ID, AESTHETIC_MODEL_FILE)
        state = torch.load(cached, map_location="cpu", weights_only=True)
        self.mlp.load_state_dict(state)
        self.dtype = dtype
        self.eval()

    @torch.no_grad()
    def __call__(self, images):
        device = next(self.parameters()).device
        inputs = self.processor(images=images, return_tensors="pt")
        inputs = {k: v.to(self.dtype).to(device) for k, v in inputs.items()}
        embed = self.clip.get_image_features(**inputs)
        embed = embed / torch.linalg.vector_norm(embed, dim=-1, keepdim=True)
        return self.mlp(embed).squeeze(1)


# ── Prompts ──────────────────────────────────────────────────────────────────
# Training prompts: the TRL reference animal list. Held-out prompts: objects /
# scenes the sampler NEVER sees during training — the objective is scored here,
# so reward overfitting to the 27 training animals shows up as a gap between
# the train and held-out means instead of silently inflating the objective.
TRAIN_PROMPTS = [
    "cat", "dog", "horse", "monkey", "rabbit", "zebra", "spider", "bird",
    "sheep", "deer", "cow", "goat", "lion", "frog", "chicken", "duck", "goose",
    "bee", "pig", "turkey", "fly", "llama", "camel", "bat", "gorilla",
    "hedgehog", "kangaroo",
]

HELDOUT_PROMPTS = [
    "castle", "waterfall", "sailboat", "lantern", "windmill", "canyon",
    "lighthouse", "butterfly",
]


def read_params() -> dict:
    """Swept hyperparameters from the campaign (ATHENA_EXPERIMENT_SPEC)."""
    try:
        return json.loads(os.environ.get("ATHENA_EXPERIMENT_SPEC", "") or "{}").get(
            "parameters"
        ) or {}
    except Exception:
        return {}


def main() -> None:
    t_start = time.time()
    params = read_params()
    athm = AthenaMetrics()

    seed = int(os.environ.get("ATHENA_SEED", "0") or 0)
    torch.manual_seed(seed)
    np.random.seed(seed)
    random.seed(seed)
    prompt_rng = np.random.default_rng(seed)

    # Swept science knobs (parameterSchema in the ExperimentTemplate). Anything
    # numeric here WILL be perturbed by the campaign strategies.
    num_epochs = int(float(params.get("num_epochs", 64)))
    learning_rate = float(params.get("train_learning_rate", 3e-4))
    sample_num_steps = int(float(params.get("sample_num_steps", 50)))
    sample_batches_per_epoch = int(float(params.get("sample_num_batches_per_epoch", 4)))
    stat_buffer = int(float(params.get("per_prompt_stat_tracking_buffer_size", 32)))

    # Hardware knobs (RuntimeProfile env, never swept): 8GB Turing cards cannot
    # absorb a x2 PBT perturbation of the batch size.
    sample_batch_size = int(os.environ.get("DDPO_SAMPLE_BATCH_SIZE", "4"))
    train_batch_size = int(os.environ.get("DDPO_TRAIN_BATCH_SIZE", "1"))
    grad_accum = int(os.environ.get("DDPO_GRADIENT_ACCUMULATION_STEPS", "2"))

    checkpoint_dir = os.environ.get("ATHENA_CHECKPOINT_DIR", "")
    resume_from = os.environ.get("ATHENA_RESUME_FROM", "")
    workspace = os.environ.get("ATHENA_WORKSPACE_PATH", "")

    if params:
        print(f"[athena] campaign parameters: {sorted(params)}", flush=True)
    journal(
        "run_start",
        seed=seed,
        num_epochs=num_epochs,
        learning_rate=learning_rate,
        sample_num_steps=sample_num_steps,
    )

    config = DDPOConfig(
        num_epochs=num_epochs,
        train_learning_rate=learning_rate,
        sample_num_steps=sample_num_steps,
        sample_num_batches_per_epoch=sample_batches_per_epoch,
        per_prompt_stat_tracking=True,
        per_prompt_stat_tracking_buffer_size=stat_buffer,
        sample_batch_size=sample_batch_size,
        train_batch_size=train_batch_size,
        gradient_accumulation_steps=grad_accum,
        # Turing has no bf16; fp16 with GradScaler via accelerate.
        mixed_precision="fp16",
        # athena owns metrics; no wandb in the loop.
        log_with=None,
    )
    config.project_kwargs = {
        "logging_dir": os.path.join(workspace or ".", "logs"),
        "project_dir": os.path.join(workspace or ".", "save"),
        "automatic_checkpoint_naming": True,
        "total_limit": 2,
    }

    pipeline = DefaultDDPOStableDiffusionPipeline(
        "runwayml/stable-diffusion-v1-5",
        pretrained_model_revision="main",
        use_lora=True,
    )
    # Memory savers for the 8GB pool: VAE decode is the activation peak.
    pipeline.sd_pipeline.enable_vae_slicing()
    if hasattr(pipeline.sd_pipeline, "enable_vae_tiling"):
        pipeline.sd_pipeline.enable_vae_tiling()

    # PBT warm start: parent LoRA weights, fresh optimizer (deliberate — the
    # child runs perturbed hyperparameters, so stale optimizer moments would be
    # wrong). Missing/corrupt resume degrades to a cold start with a loud log.
    warm_started = False
    if resume_from and os.path.isdir(resume_from):
        try:
            pipeline.sd_pipeline.load_lora_weights(resume_from)
            warm_started = True
            print(f"[athena] warm start: LoRA weights from {resume_from}", flush=True)
        except Exception as exc:  # noqa: BLE001 — degrade, never dead-run
            print(f"[athena] warm start FAILED ({exc}); cold start", file=sys.stderr, flush=True)
    elif resume_from:
        print(f"[athena] ATHENA_RESUME_FROM={resume_from} not found; cold start", file=sys.stderr, flush=True)
    journal("warm_start", resume_from=resume_from, applied=warm_started)

    scorer = AestheticScorer(dtype=torch.float32)
    if torch.cuda.is_available():
        scorer = scorer.cuda()

    train_rewards: list[float] = []

    def reward_fn(images, prompts, metadata):
        images = (images * 255).round().clamp(0, 255).to(torch.uint8)
        scores, _meta = None, {}
        scores = scorer(images)
        train_rewards.extend(scores.detach().float().cpu().tolist())
        # Live gauge: running train-reward mean (diagnostic only).
        if len(train_rewards) % (sample_batch_size * 2) == 0:
            athm.store("ddpo_train_reward_mean", float(np.mean(train_rewards)))
        return scores, {}

    def prompt_fn():
        return str(prompt_rng.choice(TRAIN_PROMPTS)), {}

    trainer = DDPOTrainer(config, reward_fn, prompt_fn, pipeline)
    trainer.train()

    # ── Held-out evaluation ──────────────────────────────────────────────────
    # The honest objective: aesthetic score on prompts the trainer never saw.
    # Fixed eval seeds per prompt so cross-experiment comparisons are paired.
    sd = trainer.pipeline.sd_pipeline
    sd.set_progress_bar_config(disable=True)
    heldout_scores: list[float] = []
    eval_samples_per_prompt = int(os.environ.get("DDPO_EVAL_SAMPLES_PER_PROMPT", "2"))
    for i, prompt in enumerate(HELDOUT_PROMPTS):
        for j in range(eval_samples_per_prompt):
            gen = torch.Generator(device="cpu").manual_seed(10_000 + 97 * i + j)
            image = sd(
                prompt,
                num_inference_steps=sample_num_steps,
                generator=gen,
                output_type="np",
            ).images[0]
            score = scorer((torch.from_numpy(image).permute(2, 0, 1) * 255).round().clamp(0, 255).to(torch.uint8).unsqueeze(0))
            heldout_scores.append(float(score.squeeze()))
    aesthetic_heldout_mean = float(np.mean(heldout_scores))
    aesthetic_train_reward_mean = float(np.mean(train_rewards)) if train_rewards else 0.0

    # ── Checkpoint (for PBT children to warm-start from) ─────────────────────
    latest_checkpoint = None
    if checkpoint_dir:
        try:
            out = os.path.join(checkpoint_dir, "lora_final")
            os.makedirs(out, exist_ok=True)
            trainer.pipeline.sd_pipeline.save_lora_weights(out)
            latest_checkpoint = out
            print(f"[athena] LoRA weights saved to {out}", flush=True)
        except Exception as exc:  # noqa: BLE001
            print(f"[athena] checkpoint save failed: {exc}", file=sys.stderr, flush=True)

    # Sample grid for the console/report: one image per held-out prompt.
    if workspace:
        try:
            from PIL import Image

            grid_dir = os.path.join(workspace, "figures")
            os.makedirs(grid_dir, exist_ok=True)
            tiles = []
            for i, prompt in enumerate(HELDOUT_PROMPTS):
                gen = torch.Generator(device="cpu").manual_seed(10_000 + 97 * i)
                img = sd(prompt, num_inference_steps=sample_num_steps, generator=gen, output_type="np").images[0]
                tiles.append(Image.fromarray((img * 255).astype(np.uint8)))
            w, h = tiles[0].size
            grid = Image.new("RGB", (w * 4, h * 2))
            for idx, tile in enumerate(tiles):
                grid.paste(tile, ((idx % 4) * w, (idx // 4) * h))
            grid.save(os.path.join(grid_dir, "heldout_grid.png"))
        except Exception as exc:  # noqa: BLE001 — viz must never fail the trial
            print(f"[athena] sample grid failed: {exc}", file=sys.stderr, flush=True)

    wall = time.time() - t_start
    summary = {
        # THE objective: held-out prompts only. The train mean is the
        # reward-hackable diagnostic.
        "aesthetic_heldout_mean": aesthetic_heldout_mean,
        "aesthetic_train_reward_mean": aesthetic_train_reward_mean,
        "aesthetic_heldout_n": len(heldout_scores),
        "num_epochs_run": num_epochs,
        "warm_started": warm_started,
        "wall_clock_seconds": wall,
        **(
            {"latest_checkpoint": latest_checkpoint, "best_checkpoint": latest_checkpoint}
            if latest_checkpoint
            else {}
        ),
        "metric_series": {
            "objective": "aesthetic_heldout_mean",
            "goal": "maximize",
            "points": [
                {"name": "aesthetic_heldout_mean", "value": aesthetic_heldout_mean, "step": num_epochs}
            ],
        },
    }
    if workspace:
        try:
            os.makedirs(workspace, exist_ok=True)
            with open(os.path.join(workspace, "metrics.json"), "w") as fh:
                json.dump(summary, fh)
        except Exception:
            pass
    try:
        with open("/dev/termination-log", "w") as fh:
            fh.write(json.dumps(summary))
    except Exception:
        pass
    journal(
        "run_end",
        aesthetic_heldout_mean=aesthetic_heldout_mean,
        aesthetic_train_reward_mean=aesthetic_train_reward_mean,
        wall_clock_seconds=wall,
    )
    athm.store("ddpo_aesthetic_heldout_mean", aesthetic_heldout_mean)
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    main()
