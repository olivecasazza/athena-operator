# Athena Integration

Mapping a physical chemostat campaign onto the CRDs:

- Parameter sweep → `ResearchCampaign`.
- One setpoint (or one hysteresis sweep) → `Experiment`. Falsifiable claim in
  `spec.hypothesis` (e.g. "oscillation amplitude collapses above D = 0.02 s⁻¹");
  the swept variable in `spec.parameters`.
- Runner: stateless Job talking to the device controller. Reports via declared
  artifacts and exit status; the operator parses and owns authoritative status.
- Raw probe traces → workspace artifacts. Normalized summaries (steady-state
  conversion, oscillation period, bifurcation point) → bounded status fields.
- Conclusions → `ResearchReport`.

Boundary: this corpus holds reference knowledge that informs specs and
hypotheses. Measurements and outcomes live in CRDs per AGENTS.md
(Research Memory) — never here.
