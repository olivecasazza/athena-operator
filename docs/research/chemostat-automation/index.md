# Chemostat Automation

Appraisal of abiotic experiments automatable with a physical chemostat-class
device. Scope excludes biology: a chemostat minus cells is a CSTR (continuous
stirred-tank reactor) with precise residence-time control — pumps in, overflow
out, stirrer, jacket, inline probes.

Why it suits autonomous experimentation: dilution rate `D = Q/V` is a clean,
continuously actuatable 1-D knob, and steady states turn kinetics into
fixed-point measurements — wait 3–5 residence times, read a probe, step the
setpoint. The device resets by flushing at high flow.

Last reviewed: 2026-08-30

## Contents

| File | Topic |
| --- | --- |
| [experiment-classes.md](experiment-classes.md) | Ranked appraisal of 10 automatable experiment classes |
| [automation-criteria.md](automation-criteria.md) | What the ranking turns on; unattended-run failure modes |
| [device-kit.md](device-kit.md) | Minimal physical build |
| [athena-integration.md](athena-integration.md) | Mapping onto Athena CRDs |

## Shortlist

1. **Process control / system ID** — burns nothing (water + heat) while the
   autonomous loop itself is debugged.
2. **Nonlinear chemical dynamics (BZ)** — deepest science per mL once the loop
   works; bifurcation mapping is a natural active-learning search.

Prototype in that order.
