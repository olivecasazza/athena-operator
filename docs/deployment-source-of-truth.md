# Deployment source of truth

Athena runs as **two Deployments, deployed by two different mechanisms, from two different
repositories**, and **no pipeline in this repository builds or publishes either image**. This
document states who owns what, where every running digest is hand-maintained, and the rules
that stop the two failure modes that have already occurred here (silent loss of the operator
OOM fix, and digests that drift apart with nothing to detect it).

**Evidence standard.** Every factual claim carries a `file:line` citation, or is marked
**[INFERENCE]**. Citations to `nixlab:` are paths in `~/Repositories/nixlab` on host `seir`.
Facts marked *live* were read from the cluster with read-only `kubectl get` on 2026-09-28.
Audit date: 2026-09-28, athena-operator `main` @ `b8fd5378afadef8a3bec6654bd5d5595147042f0`,
nixlab working tree clean on its own branch.

---

## 1. Ownership: which repository owns what

| Thing | Owned by | Where | Applied by |
|---|---|---|---|
| Helm chart templates + `values.yaml` + `Chart.yaml` | **athena-operator** | `charts/athena/` | Flux, via the nixlab GitRepository + HelmRelease |
| CRDs | **athena-operator** | `charts/athena/crds/`, copied at `nix/athena/deployment.nix:844` | Flux HelmRelease, `install.crds`/`upgrade.crds = CreateReplace` (`nixlab:modules/k8s/apps/athena.nix:38-39`) |
| **Operator image digest** | **nixlab** (hand-pinned) | `nixlab:modules/k8s/apps/athena.nix:57` | Flux |
| **Console image digest** | **nixlab** (hand-pinned) | `nixlab:modules/k8s/apps/athena-console.nix:215` | Flux (kubenix) |
| Console Deployment spec | **nixlab** | `nixlab:modules/k8s/apps/athena-console.nix:187-262` | Flux (kubenix) |
| `modules/k8s/manifests.yaml` (this repo) | **athena-operator** | `modules/k8s/manifests.yaml` | **nothing** — see §5 |

Three consequences, stated plainly:

1. **The chart comes from this repo; the images do not.** Flux clones
   `https://github.com/olivecasazza/athena-operator` at branch `main`
   (`nixlab:modules/k8s/apps/athena.nix:16-17`) via GitRepository `n-autoresearch`
   (`nixlab:modules/k8s/apps/athena.nix:11`) and installs the chart at `./charts/athena`
   (`nixlab:modules/k8s/apps/athena.nix:30`, `sourceRef` at `:31-34`). But the *image tag*
   in that HelmRelease's `values` is a hand-written string, not something this repo emits:
   `nixlab:modules/k8s/apps/athena.nix:57`. Editing `nix/athena/deployment.nix` does **not**
   change the running operator image.
2. **The console is not in this repo at all.** It is a plain kubenix Deployment written
   directly in nixlab (`nixlab:modules/k8s/apps/athena-console.nix:187`), pulled from
   `ghcr.io/olivecasazza/athena-console` (`nixlab:modules/k8s/apps/athena-console.nix:215`).
   The string `panelkit` does not appear in any deployed artifact in this repo — it is
   absent from `charts/athena/`, `nix/`, and `modules/`, and its only mentions in the
   tracked tree are the push commands at `.omp/skills/athena/SKILL.md:123-124,135`.
3. **The chart is sourced from the git tree, not from the Nix build.** `helmChart`
   (`nix/athena/deployment.nix:832-863`) produces a *separate* copy in the Nix store; Flux
   reads `charts/athena/` out of the cloned repository. That is what makes §2 possible.

---

## 2. Every running image, its digest, and who edits it

| Deployment | Image reference | Hand-maintained at | Who must edit it on rebuild |
|---|---|---|---|
| `apps/athena` (operator) | `ghcr.io/olivecasazza/athena-operator:latest@sha256:275e200eab62e0bcdbff9a8e57f6f773297f22716896e737886bb2cf3917ffd5` | `nixlab:modules/k8s/apps/athena.nix:57` — **the only place that decides what runs** | Whoever pushes `:latest` |
| `apps/athena-console` | `ghcr.io/olivecasazza/athena-console:panelkit@sha256:0f3b46addaf3e7942133189ce15fe3ad12568c80e0a54b5cc2a7d3a1f1845041` | `nixlab:modules/k8s/apps/athena-console.nix:215` | Whoever pushes `:panelkit` |
| *(derived)* nixlab rendered output | both of the above | `nixlab:modules/k8s/manifests.yaml:788` (operator) and `:812` (console) | **Never by hand** — regenerate (`.omp/skills/athena/SKILL.md:136`) |
| *(never applied)* repo manifests | `ghcr.io/olivecasazza/athena-operator:latest@sha256:caff7fb48dead330a79fbee4c8e908cbb442e2e2055d1659bea51a1e47ae25d5` | `nix/athena/deployment.nix:20`, rendered at `modules/k8s/manifests.yaml:42` | Only if you want the never-applied path to stop being wrong; nothing depends on it |
| *(chart default)* | **empty** — `tag: ''` | `charts/athena/values.yaml:18` | Keep in sync with `nix/athena/deployment.nix:20` or the chart has no default image |

Live cluster, read 2026-09-28 (read-only): `apps/athena` runs the `275e200e` digest with
`limits.memory: 1Gi`, `requests: cpu 1 / memory 512Mi`; `apps/athena-console` runs the
`0f3b46ad` digest. Both match the nixlab pins exactly.

**Correction to a prior audit.** That audit reported "at least FOUR distinct image digests"
across nixlab helm values, nixlab kubenix, `modules/k8s/manifests.yaml`, and the live
cluster. Direct observation refutes the four-way split: the live cluster agrees with the
nixlab pins byte-for-byte. There are **two load-bearing digests** (operator, console) plus
one never-applied stale operator digest (`caff7fb4`, `nix/athena/deployment.nix:20`) and one
empty chart default (`charts/athena/values.yaml:18`). The drift risk is not "four places to
keep in sync" — it is that *this repo's* copies of the operator digest are already wrong and
invisible to the cluster.

### Why nothing bumps these digests automatically

- The `# {"$imagepolicy": "apps:athena-operator"}` marker on
  `nixlab:modules/k8s/apps/athena.nix:57` (and its twin at
  `nixlab:modules/k8s/apps/athena-console.nix:215`) is a **Nix comment**. It does not survive
  generation into the rendered manifests, so Flux's image-automation has nothing to rewrite.
  This is already documented in-tree at `nixlab:modules/k8s/apps/athena.nix:52-56` and
  `.omp/skills/athena/SKILL.md:141` ("Image automation has not been reliably bumping the
  operator digest, so pin it explicitly").
- There is **no `.github/` directory** in this repository (verified: absent on disk and absent
  from the `HEAD` tree) and no other build/publish automation, so nothing in this repo can
  move a digest.
- The operator image is built by `pkgs.dockerTools.buildLayeredImage` with
  `tag = "dev"` (`flake.nix:164-166`, exposed as `flake.nix:252`) and is **never pushed by
  any Nix or GitHub action**; the push is a manual `docker tag`/`docker push`
  (`.omp/skills/athena/SKILL.md:126-128`). GitOps deploys `:latest`
  (`nixlab:modules/k8s/apps/athena.nix:57`) while the flake builds `:dev` — the same image
  under a tag this repo never deploys.
- There is no `Dockerfile` for the operator in the tracked tree (verified: `git ls-tree -r
  HEAD` lists only `Dockerfile.audit`, `.console`, `.ddpo-trainer`, `.mesh`, `.mesh-rocm`,
  `.mesh-vulkan`, `.prover`, `.sky`, `.trainer`). A `Dockerfile.athena` existed in history
  (added `921b773`, removed by `a409e84 chore: remove n-autoresearch fork-era cruft`). The
  operator image is therefore reproducible only through the Nix crane build
  (`flake.nix:158-178`), not through a Docker build. **[INFERENCE]** any registry copy of
  the operator image not produced by that derivation is not reproducible from this tree.

---

## 3. Rule 1 — `charts/athena/` is generated output; edit the generator, never the output

`helmChart` (`nix/athena/deployment.nix:832-863`) is a derivation whose `installPhase`
**overwrites** the chart: `templates/deployment.yaml` at
`nix/athena/deployment.nix:845`, `templates/service.yaml` at `:846`,
`templates/observability.yaml` at `:847`, `templates/rbac.yaml` at `:848`,
`values.yaml` at `:849`, and `Chart.yaml` at `:850-861`. CRDs are copied from the source
tree at `:844`.

**The rule: a change that must persist belongs in `nix/athena/deployment.nix`. A hand-edit
under `charts/athena/templates/` or `charts/athena/values.yaml` is a bug, even when it is the
fix that keeps production alive.**

This is not a warning about a hypothetical. The committed chart has **already** diverged
from the generator in three places, and every one of them is load-bearing:

| Committed chart says | Generator emits | Consequence if the generated output ever wins |
|---|---|---|
| `charts/athena/templates/deployment.yaml:42-43` renders `{{- toYaml .Values.resources \| nindent 12 }}` | `nix/athena/deployment.nix:634` hardcodes `resources = deployment.operator.resources` = `128Mi`/`256Mi` (`:27-35`) | The nixlab override at `nixlab:modules/k8s/apps/athena.nix:75-85` (`1Gi`/`512Mi`) is **silently ignored**; the operator reverts to 256Mi and OOM-crash-loops |
| `charts/athena/values.yaml:35-39` = `1Gi` limits, `512Mi` requests | `nix/athena/deployment.nix:784` (`helmValues.resources`) = `256Mi`/`128Mi` from `:27-35` | Chart default drops back to the size documented as OOM-killing at `charts/athena/values.yaml:32-34` |
| `charts/athena/values.yaml:18` = `tag: ''` | `nix/athena/deployment.nix:760-762` — `deployment.image // { tag = ""; }` keeps the **left** operand, i.e. `latest@sha256:caff7fb4…` from `:20` | The chart grows a hardcoded default pointing at the **wrong, never-applied digest** |

(The three generator outputs in that table are derived by reading the expressions at
`nix/athena/deployment.nix:634`, `:784`, `:760-762` and `:27-35`; building to diff them was
out of scope for this audit.)

**Why nothing catches this.** `flake.nix:258-261` defines `checks.helm-chart` and
`checks.k8s-manifests`, and `AGENTS.md:144` tells you to run `nix build .#helm-chart`. That
check only proves the derivation *builds*. It never compares the derivation's output against
the committed `charts/athena/`, so it passes identically with and without the hand-edits.
The build is therefore not a drift detector, and `AGENTS.md:77` ("Avoid editing generated
files unless the generation workflow requires it") is the rule that the chart has been
violating, silently, in the one place it protects.

### The correct way to make each change persist

- **Make `resources` configurable from Helm values.** Emit
  `{{- toYaml .Values.resources | nindent 12 }}` *from the generator* — the template string
  is the chart's own contract, so it belongs in `helmTemplates` /
  `k8sObjects` at `nix/athena/deployment.nix:593-640` and the values default belongs at
  `nix/athena/deployment.nix:784` — then raise the default in
  `nix/athena/deployment.nix:27-35` to match the 1Gi/512Mi that the cluster is verified to be
  running. That removes the hand-edit, keeps the nixlab override working, and makes
  `charts/athena/values.yaml:31-39` reproducible instead of contradicting
  `nix/athena/deployment.nix:27-35`.
- **Keep the image default honest.** Either let `nix/athena/deployment.nix:20` be the
  intended default (and accept that it is currently stale relative to
  `nixlab:modules/k8s/apps/athena.nix:57`), or drop the default to `''` in the generator so
  the chart matches `charts/athena/values.yaml:18` and cannot silently pin a wrong digest.
  Do not leave the two in disagreement.
- **Bump the chart version** in `nix/athena/deployment.nix:10` (currently `0.1.11`) whenever
  `charts/athena/` output changes; it is what `helm.sh/chart` renders
  (`nix/athena/deployment.nix:91`) and what Flux's `reconcileStrategy = "Revision"`
  (`nixlab:modules/k8s/apps/athena.nix:36`) consumes.

### The permanent fix for the drift itself

Until the hand-edits are folded into the generator, treat this as a **merge-blocking**
condition: a diff that touches `nix/athena/deployment.nix` and leaves `charts/athena/`
untouched, or a diff that edits `charts/athena/` without `nix/athena/deployment.nix`, is
broken in one direction or the other. `AGENTS.md:129` already requires chart changes to be
"generated through repo-owned Nix/Helm flows"; this is that rule made checkable.

---

## 4. Rule 2 — digest bumps are a manual, ordered, two-repo procedure

Both images are built and pushed by hand (`.omp/skills/athena/SKILL.md:116-146`). The order
matters because the athena repo and nixlab are separate Git histories and Flux reads only
nixlab.

### "I rebuilt the console image" — exact answer

The console image is built from `Dockerfile.console` (`Dockerfile.console:1-42`: Dioxus wasm
SPA via `dioxus-cli 0.6.3` at `Dockerfile.console:11`, `console-server` at
`Dockerfile.console:22`, `debian:bookworm-slim` runtime at `Dockerfile.console:31`).
**No digest appears in `Dockerfile.console`, and no deployed artifact in this repository
references the console** — the `:panelkit` tag appears only in the push commands at
`.omp/skills/athena/SKILL.md:123-124` and the pin reminder at
`.omp/skills/athena/SKILL.md:135`. Only two nixlab files matter, in this order:

1. **Build and push**, from a committed tree so the build is reproducible:
   ```bash
   git archive --format=tar HEAD | docker build -q -f Dockerfile.console \
     -t ghcr.io/olivecasazza/athena-console:panelkit -t ghcr.io/olivecasazza/athena-console:latest -
   docker push -q ghcr.io/olivecasazza/athena-console:panelkit
   docker push -q ghcr.io/olivecasazza/athena-console:latest
   ```
   (`.omp/skills/athena/SKILL.md:122-124`)
2. **Read the pushed digest** — pin the manifest digest, not the local one:
   `docker image inspect <image> --format '{{index .RepoDigests 0}}'`
   (`.omp/skills/athena/SKILL.md:129`).
3. **Edit `nixlab:modules/k8s/apps/athena-console.nix:215`** — replace the digest, keep the
   `# {"$imagepolicy": "apps:athena-console"}` trailing comment even though it is inert
   (see §2), and keep the `:panelkit` tag in the reference so the pin stays auditable
   against what was pushed.
4. **Regenerate `nixlab:modules/k8s/manifests.yaml`** (line `:812` holds the console
   Deployment) with `cp $(nix build .#k8s-manifests --no-link --print-out-paths)
   modules/k8s/manifests.yaml; chmod u+w modules/k8s/manifests.yaml`
   (`.omp/skills/athena/SKILL.md:136`, `:140`). Never hand-edit it.
5. **Commit and push both nixlab files** to `main`
   (`.omp/skills/athena/SKILL.md:137-138`).
6. **Nudge Flux and confirm** (`.omp/skills/athena/SKILL.md:143-145`):
   ```bash
   kubectl annotate gitrepository flux-system -n flux-system reconcile.fluxcd.io/requestedAt="$(date -Iseconds)" --overwrite
   kubectl annotate kustomization  flux-system -n flux-system reconcile.fluxcd.io/requestedAt="$(date -Iseconds)" --overwrite
   kubectl rollout status deploy/athena-console -n apps
   ```

### "I rebuilt the operator image" — exact answer

The operator image is a Nix crane build (`flake.nix:158-163`) packaged by
`dockerTools.buildLayeredImage` tagged **`dev`** (`flake.nix:164-166`), exposed as
`packages.athena-operator-image` (`flake.nix:252`). GitOps runs `:latest`.

1. Commit and push this repo's `main` first — Flux clones `main`
   (`nixlab:modules/k8s/apps/athena.nix:17`) and the chart is installed from that tree
   (`nixlab:modules/k8s/apps/athena.nix:30`), so the Git and image states must not be
   confused.
2. `docker load -q < $(nix build .#athena-operator-image --no-link --print-out-paths)`, then
   `docker tag ghcr.io/olivecasazza/athena-operator:dev ghcr.io/olivecasazza/athena-operator:latest`
   and `docker push -q` it (`.omp/skills/athena/SKILL.md:126-128`). The `dev`→`latest` retag
   is manual and unwatched; nothing republishes it.
3. Read the pushed digest (`.omp/skills/athena/SKILL.md:129`).
4. **Edit `nixlab:modules/k8s/apps/athena.nix:57`** — this is the only edit that changes what
   runs. Preserve the trailing `$imagepolicy` comment for the same reason as above.
5. **Also update `nix/athena/deployment.nix:20` in this repo** to the same digest. Nothing
   requires this today (`modules/k8s/manifests.yaml` is applied by nothing, §5), but leaving
   `caff7fb4…` there means the next person to fix the generator drift in §3 starts from a
   digest that is not the running one.
6. Regenerate `nixlab:modules/k8s/manifests.yaml` (`.omp/skills/athena/SKILL.md:136`) and
   regenerate this repo's `modules/k8s/manifests.yaml` with
   `nix build .#k8s-manifests` (`AGENTS.md:130`, `:174`).
7. Flux nudge + `kubectl rollout status deploy/athena -n apps`
   (`.omp/skills/athena/SKILL.md:143-145`).

### Ordering rule

Always: **push image → pin digest in nixlab → regenerate derived manifests → commit/push
nixlab → nudge Flux → verify rollout.** Never the reverse: a nixlab pin that names a digest
you have not pushed yet leaves the Deployment stuck in `ImagePullBackOff`, and the operator
chart uses `pullPolicy: IfNotPresent` (`nixlab:modules/k8s/apps/athena.nix:58`; chart default
`charts/athena/values.yaml:16`) so the same digest must be reused, never re-tagged in place.

---

## 5. Rule 3 — `modules/k8s/manifests.yaml` in this repo is a reference artifact

`k8sManifests` (`nix/athena/deployment.nix:865-884`) concatenates the operator's k8s objects
with the example canaries into a single file, which the `generate-k8s-manifests` pre-commit
hook copies into `modules/k8s/manifests.yaml` (`nix/athena/deployment.nix:873-875`).

**Nothing applies it.** Verified: this repository contains no Flux or Kustomization config
(`git ls-files` matches nothing for `flux` or `kustomization`), and in nixlab the only
references to the athena GitRepository are the GitRepository and HelmRelease in
`nixlab:modules/k8s/apps/athena.nix:11,33`. The only path Flux takes into this repo is the
Helm chart at `./charts/athena` (`nixlab:modules/k8s/apps/athena.nix:30`).

Its practical consequences:

- It is a **stale mirror**: it carries the operator image `caff7fb4…`
  (`modules/k8s/manifests.yaml:42`) and 256Mi resources
  (`modules/k8s/manifests.yaml:49-54`), neither of which matches the cluster.
- It is a **trap**: editing it has no deployment effect, and anyone who reads it to learn
  "what is deployed" will be wrong about the image and the memory limit.
- It must still be **regenerated, never hand-resolved** (`AGENTS.md:130`, `:174`), and the
  commitizen message for the nixlab side is `feat(athena): …`
  (`.omp/skills/athena/SKILL.md:137`).

---

## 6. Pre-merge rule sets (checkable)

### A. The change touches the operator image digest

- [ ] The digest is pinned at `nixlab:modules/k8s/apps/athena.nix:57` and nowhere else in
      nixlab except the regenerated `nixlab:modules/k8s/manifests.yaml:788`.
      Verify: `cd ~/Repositories/nixlab && git grep -n '275e200eab62e0bc'`
- [ ] `nix/athena/deployment.nix:20` names the **same** digest, or the PR explicitly states it
      is intentionally left stale and why (§4, step 5).
- [ ] The `# {"$imagepolicy": "apps:athena-operator"}` trailing comment survives on
      `nixlab:modules/k8s/apps/athena.nix:57` (`nixlab:modules/k8s/apps/athena.nix:52-56`
      explains it is inert but load-bearing for tooling archaeology).
- [ ] `nixlab:modules/k8s/manifests.yaml` was **regenerated**, not hand-edited
      (`.omp/skills/athena/SKILL.md:136`).
- [ ] The image was pushed *before* the pin, and `pullPolicy: IfNotPresent`
      (`nixlab:modules/k8s/apps/athena.nix:58`) means the same digest will not be re-pushed.
- [ ] The PR says how it was verified: `kubectl rollout status deploy/athena -n apps`
      (`.omp/skills/athena/SKILL.md:145`) — not "CI passed", since there is no CI
      (§2, no `.github/`).
- [ ] If the change also touches CRDs: remember `install.crds` and `upgrade.crds` are both
      `CreateReplace` (`nixlab:modules/k8s/apps/athena.nix:38-39`), so a chart change that
      touches `charts/athena/crds/` **replaces** live CRDs on every upgrade, under a `20m`
      timeout (`nixlab:modules/k8s/apps/athena.nix:44`) that previously failed
      (`:40-43`).

### B. The change touches the chart

- [ ] **The change is in `nix/athena/deployment.nix`, not in `charts/athena/`.** Confirm with
      `git status --porcelain` and `git diff --stat -- charts/athena` — a chart-only diff is
      merge-blocking unless §3's "correct way" was followed and the generator produces the
      same bytes.
- [ ] `charts/athena/templates/deployment.yaml:42-43`, `charts/athena/values.yaml:31-39` and
      `charts/athena/values.yaml:18` are **unchanged by hand**; if the intent was to change
      resources, the edit is at `nix/athena/deployment.nix:27-35` and
      `nix/athena/deployment.nix:784`; if the intent was to make resources Helm-configurable,
      the template string is emitted from `nix/athena/deployment.nix:593-640`.
- [ ] `chart.version` was bumped at `nix/athena/deployment.nix:10`.
- [ ] `nix build .#helm-chart` and `nix fmt` were run (`AGENTS.md:144-145`, `:166`) — with
      the knowledge that this proves the build works, **not** that the committed chart
      matches the generator (§3). If drift matters to the PR, diff
      `nix/athena/deployment.nix`'s output against `charts/athena/` explicitly.
- [ ] A CRD change is a deliberate CRD change: the chart ships `charts/athena/crds/`
      (copied at `nix/athena/deployment.nix:844`) and they are replaced, not patched
      (`nixlab:modules/k8s/apps/athena.nix:38-39`).

### C. The change touches the console image

- [ ] The digest is pinned at `nixlab:modules/k8s/apps/athena-console.nix:215` and the
      regenerated `nixlab:modules/k8s/manifests.yaml:812`.
- [ ] The build came from `Dockerfile.console:1-42` via a committed tree (`git archive HEAD`,
      `.omp/skills/athena/SKILL.md:122`) — **not** from a dirty working tree, since
      `Dockerfile.console:14` copies the whole build context.
- [ ] The `:panelkit` tag was retained in the pinned reference, so the digest in nixlab is
      traceable to the tag that was pushed.
- [ ] **No file in this repository was edited for the console change** — if one was, the
      change was misfiled (there is no console reference in the athena repo's chart,
      templates, or values: `charts/athena/values.yaml:1-42` is operator-only).
- [ ] Verified with `kubectl rollout status deploy/athena-console -n apps`
      (`.omp/skills/athena/SKILL.md:145`); `imagePullPolicy: Always`
      (`nixlab:modules/k8s/apps/athena-console.nix:216`) means the rollout exercises the new
      digest without a node-side pull barrier.
- [ ] If console scheduling changed, remember the pinned `nodeSelector."nixlab.io/pool" =
      "cpu-pool"` and its explicit "REVERT when the mac minis return" note
      (`nixlab:modules/k8s/apps/athena-console.nix:200-207`); the operator has a matching,
      separately-commented placement (`nixlab:modules/k8s/apps/athena.nix:87-95`).

---

## 7. Rules that are policy, not mechanism

- **Pin digests, never tags** — `AGENTS.md:136`; that is the whole reason the `$imagepolicy`
  markers exist and why the hand-pins in §2 are not "temporary".
- **No imperative cluster writes for deployment changes** — `AGENTS.md:128`, `:180`. Every
  deployed change must be a Git/Nix/Flux change.
- **Never edit generated output** — `AGENTS.md:77`, `:171-175`. `charts/athena/` is in scope
  of that rule even though `AGENTS.md:171-175` names only `.pre-commit-config.yaml`,
  `modules/k8s/manifests.yaml` and `result*` paths. Extend the list.
- **Never use `kubectl apply`/`patch`/`edit` to "fix" a drifted chart** — that hides the
  generator drift and leaves the next `nix build` to reintroduce it.

## 8. What this document does not claim

- It does not propose a CI pipeline, a digest-publishing job, or a migration to
  digest-from-source. None exists (§2) and none is required by the rules above.
- It does not assert that the `caff7fb4…` digest corresponds to any particular source commit.
- It does not assert that `result`, `result-chart` or `result-k8s` in the working tree are
  current; they are Nix outputs and per `AGENTS.md:175` must not be edited.
- `modules/k8s/manifests.yaml` is regenerable but its *staleness* is documented here, not
  fixed: fixing it is a one-line change to `nix/athena/deployment.nix:20` that only makes
  sense alongside the §3 generator fix.
