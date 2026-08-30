# Automation Criteria

What the ranking in [experiment-classes.md](experiment-classes.md) turns on.

- **Inline observability is the whole game.** Probe-readable (conductivity, pH,
  ORP, DO, turbidity, UV-Vis, temperature) → automatable. Needs offline assay
  (ICP, HPLC, SEM) → human back in the loop; score collapses.
- **Steady state beats transient.** Fixed points tolerate timing jitter and
  restarts. Exception worth having: bistability studies *require*
  path-dependent protocols (sweep `D` up vs down, detect hysteresis) — itself a
  cleanly automatable protocol distinction.
- **Reagent economics.** Continuous operation eats feedstock proportional to
  runtime. BZ and control-theory runs are cheap; nanoparticle feeds are not.

## Unattended-run failure modes

- Tubing fouling / clogging (crystallization, nanoparticle synthesis).
- Electrode drift (pH, ORP) — needs periodic auto-calibration.
- Gas locks in pump lines.
