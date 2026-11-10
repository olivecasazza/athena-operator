# Spot learning: models and evaluation contracts

Last reviewed: 2026-11-10

This is reference knowledge for designing Athena specs. Measurements, experiment
outcomes, and decisions belong in ResearchCampaign / Experiment / ResearchReport
resources, not here.

## Candidate models and runtimes

### CPG–ENN: the current new architecture candidate

[A Trainable CPG Framework for Quadruped Locomotion](https://arxiv.org/html/2610.36537v1),
Liu et al., submitted 2026-10-27, separates an explicit eight-neuron rhythm
generator, a leg-shared supervised execution network, and a six-dimensional RL
feedback policy. The 17-input / 3-output execution MLP is trained against offline
IK joint targets and frozen during policy optimization. The paper evaluates five
gaits on Unitree Go2, not this repository's Spot model.

This is relevant because it constrains exploration to meaningful trajectory
variables rather than unconstrained joint targets. It is **not** a verified
pretrained Spot policy. Code availability, weight licensing, and reproducible
released checkpoints were not established by the paper text reviewed. Adoption
requires Spot-specific IK supervision, joint-order/sign/limit mapping, geometry
and actuator calibration, and a versioned observation/action contract. Never
warm-start an existing direct-joint checkpoint into that six-action actor.

The paper's Table IV contrasts clipped Gaussian with tanh-squashed Gaussian and
Beta policies. Treat its reported improvement as a hypothesis about consistent
bounded-action handling, not proof that changing our distribution alone helps.
Its transition RMSE is conditional on successful trials; report success rates
alongside error rather than allowing survivor-only metrics to rank policies.

### Open-DiffLoco / SHAC / JAVE: implementable, but not an automatic upgrade

[Opat, 2026-08-03](https://arxiv.org/html/2608.02069v1) releases
[Go2 training and deployment code](https://github.com/MartinOpat/open-diffloco)
for MJX differentiable simulation, SHAC, and Jacobian-Augmented Value Estimation.
These are MLP policies, not a general-purpose pretrained quadruped transformer.
JAVE supervises critic Jacobians as well as values: accurate value MSE alone
does not establish accurate actor gradients. Its reported sample-efficiency
gain is not an equivalent wall-clock gain, and the paper explicitly warns that
its PPO and first-order reward scales differ.

This requires a differentiable simulator/robot port, not a new policy class
plugged into Rapier. Go2 deployment does not establish Spot compatibility. The
paper's sub-6-GB result on an RTX 5080 does not establish throughput or memory
headroom on an 8-GB RTX 4000.

The [released software license](https://github.com/MartinOpat/open-diffloco/blob/main/LICENSE.md)
is **non-commercial copyleft**, including restrictions on internal for-profit
use and same-license distribution of derivatives. Do not vendor or publish a
derived trainer under assumed permissive terms. No code or weights from this
project are integrated here. A permissible-use decision and a dedicated,
digest-pinned runtime are prerequisites to an implementation trial.

### MuJoCo Playground: a runnable independent baseline, not a drop-in actor

The [canonical repository at revision 66640404324ddfefd0223359a7b15974c22d6d63](https://github.com/google-deepmind/mujoco_playground/tree/66640404324ddfefd0223359a7b15974c22d6d63)
was updated 2026-11-09. Its current main is a PyTorch / reaf-sim / MuJoCo Warp
stack; original JAX/MJX environments are now under `rss2025/`. Old tutorials and
`_src/locomotion/spot` links at repository root are superseded.

The [archived Spot joystick task](https://github.com/google-deepmind/mujoco_playground/blob/66640404324ddfefd0223359a7b15974c22d6d63/rss2025/mujoco_playground/_src/locomotion/spot/joystick_gait_tracking.py)
provides gait conditioning, three frames of joint-target-error history, default
pose plus scaled 12-joint position actions, and 20 ms control steps. These are
useful baseline design choices, not evidence its weights match Rapier's robot.
Joint semantics, model dimensions, mass/inertia, PD gains, observation noise and
normalization must be checked before transfer. Rapier's active v85 profile sets
`SPOT_VARIABLE_IMPEDANCE=1`: its action is **16-D** (12 joint targets plus four
per-leg stiffness controls), not the archived task's 12-D action. Matching joint
counts or 20 ms control steps is insufficient compatibility evidence.

Current main documents Python 3.12–3.13, CUDA 12/13 installation routes, and
Warp 1.18.0 CUDA-12 wheels. Do not silently upgrade the existing Python/Torch /
CUDA-12.1 trainer to that stack. Establish driver, compute capability, memory,
and image provenance in a separately bounded RuntimeProfile first. Titan Black
(sm_35) is not a target for these modern CUDA stacks. Training code is available;
a compatible published pretrained Spot checkpoint was not verified.

## Supervised signals versus task evidence

The existing Spot trainer is PPO. Its joint-space imitation term is reward
shaping; neither training reward nor imitation MSE is independent validation.
For a future supervised execution network or teacher/student model:

- Split by complete trajectories, command regimes, terrain seeds, and robot
  configuration—not randomly by adjacent timesteps from the same trajectory.
- Report per-joint angle RMSE in radians and Cartesian foot-target error, including
  worst-leg/upper-quantile errors and joint-limit violations. Publish split and
  normalization provenance; averaging across joints can hide a bad actuator.
- Retain independent closed-loop completion, fall rate, command-speed error,
  contact/gait quality, and perturbation outcomes. Low action MSE does not prove
  stability, heading control, or sim-to-real transfer.
- Compare candidate and incumbent under the same simulator/robot hash, immutable
  evaluator image, seed list, command schedule, horizon, and compute budget.
  Record failures in the denominator. Pair quality terms within episodes before
  aggregation; do not multiply independently aggregated marginal statistics.
- Keep body-frame joystick tracking distinct from spawn-heading progress. A
  controller that turns can satisfy the former while failing the latter. Define
  the commanded frame explicitly; a new scoring version does not repair a
  mismatched training reward.

## Execution boundary

Use a new, bounded Athena campaign with an explicit falsifiable hypothesis for
an architecture or metric-contract change. Preserve old results under their
original contracts. A scorer contract probe establishes engineering behavior,
not a policy-improvement result. Do not unpause broad drives, spend GPU budget,
change physical hardware control, or promote a checkpoint merely because a
new paper or a unit test looks promising.
