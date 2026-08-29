#!/usr/bin/env python3
"""Render the (morphology x stage) curriculum ExperimentTemplates + campaigns.

The curriculum trains 4 morphologies through 3 single-agent stages. Each
(morphology, stage) pair is its own research line — its own objective history,
campaign, seed chain and dashboard — exactly like the existing per-line spot
templates (spot-walk-pbt, spot-walk-speed-v85, spot-forage-tyangpu-v84).

That means 12 ExperimentTemplates which differ only in name and
`spec.defaults`. They are generated rather than hand-maintained so the
objective/metric/dashboard definition for a stage exists once: editing a stage
here updates all 4 morphologies, instead of four copies drifting apart.

Morphology, stage mode and terrain are pinned through `spec.defaults` as STRING
parameters. ExperimentTemplate has no command field, so argv cannot vary per
line; and the campaign reconciler perturbs only NUMERIC parameters, so strings
pass through untouched to the runner (train_curriculum.py reads `robot`,
`mode`, `terrain` from ATHENA_EXPERIMENT_SPEC).

    python scripts/render-curriculum.py > examples/spot-curriculum-lines.yaml
    kubectl apply -f examples/spot-curriculum-lines.yaml
"""

from __future__ import annotations

import sys

PROFILE = "spot-curriculum-trainer"
GIT_URL = "https://github.com/olivecasazza/skypilot-env"
GIT_REF = "feat/spot-ground-speed-reward"

MORPHOLOGIES = ["spider", "snake", "humanoid", "spot"]

# Per-stage: objective metric, terrain, and the extra dashboard rows that are
# meaningful for that stage. Every stage also reports the shared upright/fall/
# rig-audit rows appended below.
STAGES = {
    "stance": {
        "order": 1,
        "objective": "eval_upright_frac",
        "terrain": "heightfield",
        "rig_carry": 0.5,
        "max_time": 8.0,
        "timesteps": 3_000_000,
        "extra_metrics": {
            "eval_body_height_mean": ("Mean body height, untethered eval", "m", None),
        },
        "question": (
            "Can this morphology hold a stable upright stance, and does an "
            "elastic sky-rig carrying part of its body weight get it there "
            "faster than unassisted training? Selection is on upright fraction "
            "measured with the rig DISARMED, so tethered standing earns nothing."
        ),
    },
    "locomotion": {
        "order": 2,
        "objective": "eval_track_score",
        "terrain": "mixed",
        "rig_carry": 0.0,
        "max_time": 12.0,
        "timesteps": 5_000_000,
        "extra_metrics": {
            "eval_track_score": ("Command-tracking score (objective)", "score", "maximize"),
            "eval_fwd_speed_mps": ("Realized speed along command", "m/s", "maximize"),
        },
        "question": (
            "Seeded from this morphology's stance winner. Can a robot that can "
            "stand learn to track a commanded planar velocity and yaw rate "
            "across terrain? Selection is on integrated tracking score, which "
            "rewards tracking quality AND survival, rather than raw speed — "
            "which a diving policy maximizes."
        ),
    },
    "forage": {
        "order": 3,
        "objective": "eval_collected",
        "terrain": "foraging",
        "rig_carry": 0.0,
        "max_time": 20.0,
        "timesteps": 5_000_000,
        "extra_metrics": {
            "eval_collected": ("Batteries collected per episode (objective)", "count", "maximize"),
            "eval_episode_duration_s": ("Episode duration, untethered eval", "s", None),
        },
        "question": (
            "Seeded from this morphology's locomotion winner. Can a robot that "
            "can walk learn to SEEK — navigate to and collect battery "
            "waypoints? This is the last single-agent stage and the direct "
            "precursor to the arena, where seeking becomes pursuit (hunt) and "
            "its inverse becomes evasion."
        ),
    },
}

SHARED_METRICS = {
    "eval_upright_frac": ("Upright fraction, untethered eval", "fraction", "maximize"),
    "eval_fall_rate": ("Fall rate, untethered eval", "fraction", "minimize"),
    "eval_rig_assist_frac": ("Rig assist during eval (MUST be 0 — audit)", "fraction", "minimize"),
    "reward_mean": ("Episode reward mean (diagnostic — farmable)", "score", None),
}


def emit_template(robot: str, stage: str, cfg: dict) -> str:
    metrics = dict(SHARED_METRICS)
    metrics.update(cfg["extra_metrics"])
    rows = []
    for key, (label, unit, goal) in metrics.items():
        row = [f"      {key}:", f"        label: {label}"]
        if unit:
            row.append(f"        unit: {unit}")
        if goal:
            row.append(f"        goal: {goal}")
        rows.append("\n".join(row))
    dashboard_metrics = "\n".join(rows)

    return f"""---
apiVersion: research.nixlab.io/v1alpha1
kind: ExperimentTemplate
metadata:
  name: curriculum-{robot}-{stage}
  namespace: apps
  labels:
    research.nixlab.io/curriculum-robot: {robot}
    research.nixlab.io/curriculum-stage: {stage}
    research.nixlab.io/curriculum-order: "{cfg['order']}"
spec:
  runtimeProfileRef: {PROFILE}
  source:
    git:
      url: {GIT_URL}
      ref: {GIT_REF}
  researchObjective: >-
    Curriculum stage {cfg['order']} ({stage}) for the {robot} morphology.
    {cfg['question']}
  objective:
    metric: {cfg['objective']}
    goal: maximize
  metrics:
    parser:
      type: file
      path: metrics.json
  # Pinned identity of this research line. Strings are never perturbed by a
  # search strategy, so these stay fixed while numeric knobs below are explored.
  defaults:
    robot: {robot}
    mode: {stage}
    terrain: {cfg['terrain']}
  parameterSchema:
    rig_carry:
      type: number
      default: {cfg['rig_carry']}
      description: >-
        Fraction of body weight the elastic sky-rig carries, as a constant
        upward force. Annealed toward 0 in-env once an episode clears the
        upright competence gate, so this is the STARTING support, not a
        permanent crutch. Eval always runs at 0.
    terrain_difficulty:
      type: number
      default: 0.3
      description: Terrain roughness 0..1 for the shared terrain registry.
    max_time:
      type: number
      default: {cfg['max_time']}
      description: Episode length in seconds.
    total_timesteps:
      type: number
      default: {cfg['timesteps']}
      description: Training budget in env steps.
  dashboard:
    title: "Curriculum {cfg['order']}/{stage} — {robot}"
    metrics:
{dashboard_metrics}
"""


def emit_stage1_campaign(robot: str) -> str:
    """Stage-1 campaign, canary-gated.

    The canary is not optional ceremony: this trainer has never produced a
    trained policy on a GPU, and the repo's own history is full of long runs
    burned on recipes a short canary would have vetoed. One cheap stance run
    must succeed before the campaign spends real budget.
    """
    return f"""---
apiVersion: research.nixlab.io/v1alpha1
kind: ResearchCampaign
metadata:
  name: curriculum-{robot}-stance
  namespace: apps
  labels:
    research.nixlab.io/curriculum-robot: {robot}
    research.nixlab.io/curriculum-stage: stance
spec:
  templateRef: curriculum-{robot}-stance
  concurrency: 1
  strategy:
    type: heuristic
  budget:
    maxExperiments: 6
    maxDuration: 12h
  canary:
    parameters:
      total_timesteps: 400000
    maxDuration: 2h
"""


ARENA_TEMPLATE = f"""---
apiVersion: research.nixlab.io/v1alpha1
kind: ExperimentTemplate
metadata:
  name: curriculum-arena
  namespace: apps
  labels:
    research.nixlab.io/curriculum-stage: arena
    research.nixlab.io/curriculum-order: "4"
spec:
  runtimeProfileRef: {PROFILE}
  source:
    git:
      url: {GIT_URL}
      ref: {GIT_REF}
  researchObjective: >-
    Curriculum stage 4 (arena) — the multi-agent top of the hierarchy, and the
    only stage that is ONE research line rather than one per morphology: a round
    mixes morphologies, so predator and prey co-evolve against each other across
    bodies. Each morphology gets its own hunt and evade policy
    (<morph>_predator / <morph>_prey), matching what robot.json already declares
    per bundle and what the browser gym loads per robot; every policy
    warm-starts from a forage winner via seedExperimentRef. Selection is on
    arena_predator_return_mean because prey return is trivially farmed by
    surviving a timeout, whereas a predator only scores by closing distance and
    catching. Watch BOTH per-role series: a rising pooled return can hide
    predators collapsing while prey run out the clock.
  objective:
    metric: arena_predator_return_mean
    goal: maximize
  metrics:
    parser:
      type: file
      path: metrics.json
  defaults:
    mode: arena
    terrain: flat
  parameterSchema:
    arena_robots:
      type: number
      default: 6
      description: Robots per round; hunter fraction is sampled below 50%.
    max_time:
      type: number
      default: 20.0
      description: Round length in seconds.
    terrain_difficulty:
      type: number
      default: 0.0
      description: Terrain roughness 0..1.
    total_timesteps:
      type: number
      default: 6000000
      description: >-
        Training budget in env steps. Higher than the single-agent stages: an
        adversarial game needs enough interaction for both sides to adapt.
  dashboard:
    title: "Curriculum 4/arena — predator vs prey (all morphologies)"
    metrics:
      arena_predator_return_mean:
        label: Predator return (objective)
        unit: score
        goal: maximize
      arena_prey_return_mean:
        label: Prey return (adversary — expected to fall as predators improve)
        unit: score
      arena_policies_trained:
        label: Hunt+evade policies trained
        unit: count
      policies_exported:
        label: ONNX policies published
        unit: count
      reward_mean:
        label: Pooled episode return (diagnostic — hides per-role collapse)
        unit: score
"""


def main() -> None:
    out = [
        "# GENERATED by scripts/render-curriculum.py — edit that, not this file.",
        "#",
        "# 4 morphologies x 3 single-agent curriculum stages = 12 research lines,",
        "# plus the canary-gated stage-1 campaigns that start the hierarchy.",
        "# Requires examples/spot-curriculum.yaml (the shared RuntimeProfile).",
    ]
    for stage, cfg in sorted(STAGES.items(), key=lambda kv: kv[1]["order"]):
        for robot in MORPHOLOGIES:
            out.append(emit_template(robot, stage, cfg))
    out.append(ARENA_TEMPLATE)
    for robot in MORPHOLOGIES:
        out.append(emit_stage1_campaign(robot))
    sys.stdout.write("\n".join(out) + "\n")


if __name__ == "__main__":
    main()
