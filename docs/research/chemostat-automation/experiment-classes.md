# Experiment Classes, Ranked

Automation fitness for a chemostat-class CSTR. All abiotic. Scoring criteria:
[automation-criteria.md](automation-criteria.md).

| # | Class | Actuated knobs | Inline observable | Fitness |
|---|---|---|---|---|
| 1 | **Nonlinear chemical dynamics** — Belousov–Zhabotinsky, chlorite–iodide, iodate–arsenous acid | flow rate, feed ratios, temperature | Pt redox (ORP) electrode @ ~1 Hz; camera colorimetry | Best in class. The CSTR is the canonical instrument for oscillations, bistability, hysteresis, period-doubling, chaos. Flow rate is the bifurcation parameter; mapping a bifurcation diagram is a natural active-learning search. Richest hypothesis space per mL. |
| 2 | **Steady-state kinetics** — ethyl acetate + NaOH saponification | dilution rate, temperature, feed concentrations | conductivity | Textbook, safe, cheap. Conversion vs `D` → rate constants; temperature sweeps → activation energy. One scalar readout per steady state. |
| 3 | **Residence-time distribution / mixing** — salt or dye tracer pulse | pulse injection, stir speed, flow | conductivity or absorbance vs time | Minutes per run, water + salt only. Doubles as device self-characterization — run first to calibrate the reactor before trusting any kinetics. |
| 4 | **Process control & system ID** — step responses, PID/MPC tuning, disturbance rejection on temp/pH/level | heater, pumps, acid/base feeds | thermocouple, pH, level | No interesting chemistry required (water works). Unlimited safe repetitions. Ideal for validating the autonomous loop before spending reagents. |
| 5 | **Photochemistry** — photocatalytic dye degradation (methylene blue + TiO₂), actinometry | LED intensity/wavelength, flow, catalyst loading | inline UV-Vis flow cell | Benign reagents; electrically actuated light is a perfect knob, absorbance a perfect observable. |
| 6 | **Nanoparticle synthesis** — Ag/Au NPs; residence time controls size | flow ratio, residence time, temperature, reductant conc. | UV-Vis plasmon peak position/width | Strong self-driving-lab precedent; closed-loop "hit target spectrum" campaigns. Tubing fouling is the practical enemy. |
| 7 | **Mineral dissolution** (geochemists' "mixed-flow reactor") — acid over crushed calcite/olivine | feed pH, flow, temperature | pH, conductivity (carbonates); full rate laws need offline ICP | Published-science territory; partial inline observability drags the score. |
| 8 | **Precipitation / MSMPR crystallization** — nucleation & growth vs supersaturation and residence time | feed ratios, temperature, residence time | turbidity/NTU | Automatable, but particle-size distribution wants FBRM/laser scattering; turbidity alone is coarse. Clogging risk. |
| 9 | **Gas–liquid mass transfer** — kLa via dynamic gassing in/out (O₂/N₂, purely physical) | gas flow, stir speed, sparger | dissolved-O₂ probe | Simple, safe, fully inline; small parameter space exhausts quickly. |
| 10 | **Electrochemical flow reactor** — electro-oxidation of dyes, electrocoagulation | current density, flow | absorbance, cell voltage | Good observables; electrode passivation makes long unattended campaigns flaky. |
