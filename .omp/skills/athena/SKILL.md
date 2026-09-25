---
name: athena
description: Operate the Athena research platform — query campaigns/experiments/reports/drives, search prior art, curate ResearchReports safely, diagnose idle campaigns, verify console changes, and ship operator/console images through nixlab GitOps. Use for any Athena research, console, or deployment task in this repo.
---

# Athena

Athena's product API is Kubernetes (`research.nixlab.io/v1alpha1`, namespace `apps`). The CRDs are the research record. Read `AGENTS.md` for the full law; the parts that bite in practice:

- **Never `kubectl delete`** a campaign, experiment, drive, or report that produced a measurement. Retire work by budget or by dropping a template from a drive.
- **Never write `status`.** Agents and the console write specs only; controllers own status.
- **Deployed resources change through nixlab GitOps**, never `kubectl apply/patch/edit` of product manifests. Research records (Experiment, ResearchCampaign, ResearchReport) are created with `kubectl create -f`.
- **Conclusions go in a `ResearchReport`**, not chat or commit messages.

## Resources

| Kind | Short | What to read |
|---|---|---|
| ResearchDrive | `rsd` | `spec.paused`, `status.phase`, `status.curriculum.currentStage`, `stageHistory[].templateProgress` |
| ResearchCampaign | `rcp` | `spec.templateRef`, `spec.strategy.type`, `status.phase`, `succeeded/failed/runningExperiments`, `bestExperiment`, `bestObjective`, `conditions` (`ExperimentsHealthy`, `InferenceReady`) |
| Experiment | `exp` | `spec.campaignRef`, `spec.hypothesis`, `spec.lineage.{parent,generation}`, `status.phase`, `status.decision` (Keep/Discard/NeedsReview), `status.metricsDetail.{objectiveName,objectiveGoal,best}` |
| ResearchReport | `rrp` | `spec.sections` (map), `spec.seededHypotheses`, `spec.excludedExperiments`, `spec.references`, `status.{phase,includedCount,datasetUri,conditions}` |
| ExperimentTemplate | `ext` | `spec.objective.{metric,goal}`, `spec.runtimeProfileRef` |

Experiments have no template ref; it is their campaign's `spec.templateRef`. `status.cost` is currently never populated. `spec.parameters.mode` is a robot-trainer parameter (stance/locomotion/forage/arena), not a universal field.

## Read: the normalized snapshot (preferred)

The console server joins all of the above into one JSON document with the universal fields already resolved (template via campaign, objective value, decision, lineage, runtime from the Job window, created dates):

```bash
kubectl -n apps port-forward svc/athena-console 3999:80 &   # or run it locally, below
curl -s localhost:3999/api/snapshot | jq '.campaigns[] | {name, drive, template, phase, succeeded, failed, best_experiment, objective_value}'
curl -s localhost:3999/api/snapshot | jq '.experiments[] | select(.campaign=="citation-audit") | {name, phase, decision, objective, objective_value, parent}'
curl -s localhost:3999/api/reports/apps/<report>          # full spec + resourceVersion + status
curl -s localhost:3999/api/scheduling                     # Kueue pools/workloads, node power, inference
```

Timestamps in the snapshot are epoch-millis strings.

Raw kubectl equivalents:

```bash
kubectl get rcp -n apps --sort-by=.metadata.creationTimestamp
kubectl get exp -n apps -l athena.nixlab.io/campaign=<campaign>
kubectl get rcp <campaign> -n apps -o jsonpath='{.status.phase}{"\n"}{range .status.conditions[*]}{.type}={.status} {.reason}: {.message}{"\n"}{end}'
```

## Prior art before running anything

Search hypotheses and report text for the robot/stage/topic first; a recorded failure is the most valuable hit.

```bash
curl -s localhost:3999/api/snapshot | jq -r '.experiments[] | select(.hypothesis // "" | test("recover"; "i")) | "\(.name)\t\(.phase)\t\(.decision)\t\(.hypothesis)"'
curl -s localhost:3999/api/snapshot | jq -r '.reports[] | select((.sections|tostring) | test("footgun|fall"; "i")) | "\(.name)\t\(.title)"'
```

## Write: ResearchReport specs

Use the console API; it enforces the safe semantics.

- `POST /api/reports` creates. The name must be a DNS label, the campaign must exist, and an existing name returns 409.
- `PUT /api/reports/{ns}/{name}` replaces the spec. It **must** carry the `resource_version` you loaded, and a concurrent change returns 409. Reload, reapply, retry — never force.
- `POST /api/reports/preview` composes the dossier from an unsaved spec and writes nothing. Preview before saving.

```bash
curl -s localhost:3999/api/reports/apps/<name> | jq '.spec' > /tmp/r.json          # includes resource_version
# edit /tmp/r.json (sections are a map; keep references/about as loaded)
curl -s -X POST -H 'content-type: application/json' --data @/tmp/r.json localhost:3999/api/reports/preview | head
curl -s -X PUT  -H 'content-type: application/json' --data @/tmp/r.json localhost:3999/api/reports/apps/<name>
```

Drive-authored reports use the sections `Findings`, `Method`, `Footguns`, and `Limitations`. Record negative results and footguns explicitly.

## Diagnose

| Symptom | Check |
|---|---|
| Campaign `Running`, 0 experiments | `InferenceReady` condition; `kubectl get rayjob -n apps vllm-<campaign>`. A failed RayJob gives phase `InferenceFailed`; deleting the RayJob retries it. |
| Drive not proposing | `spec.paused`, `status.phase` (NeedsHuman = stagnation), stage gate evidence in `stageHistory` |
| GPU work pending | `/api/scheduling`, `kubectl get workloads -n apps`, ClusterQueues `hp-gpu`/`kepler-gpu`/`ada-gpu` |
| Controller behaviour | `kubectl logs deploy/athena -n apps --since=10m` (JSON lines; `reconciled ResearchCampaign`) |

If local `kubectl` fails with `ServiceUnavailable` on seir, seir's own k3s may be restarting. Contra's API server still works: `kubectl --server=https://contra:6443 --tls-server-name=127.0.0.1 …`.

## Console: verify changes against the real cluster

The public console (`https://athena-console.casazza.io`) is behind Cloudflare Access. To verify UI changes, run the built image locally with your kubeconfig. It writes to the cluster only if you click save/create.

```bash
git archive --format=tar HEAD | docker build -q -f Dockerfile.console -t athena-console:verify -
docker run -d --rm --name athena-verify --network host -v $HOME/.kube/config:/root/.kube/config:ro \
  -e ATHENA_CONSOLE_ADDR=127.0.0.1:3999 athena-console:verify
# browse http://127.0.0.1:3999 ; docker rm -f athena-verify when done
```

Views live in the top bar (Research, Experiments, Campaigns, Reports, Catalog). Tables are panel-kit `DataTable`s with filter, sort, and a columns menu.

Fast checks:

```bash
cd operator
cargo check -p athena-console-web --features server
cargo check -p athena-console-web --target wasm32-unknown-unknown
cargo test -p athena-console-web --lib
cargo test -p athena
```

## Ship: images through nixlab GitOps

1. Commit and push `athena-operator` `main`.
2. Build and push the images:
   ```bash
   # console
   git archive --format=tar HEAD | docker build -q -f Dockerfile.console \
     -t ghcr.io/olivecasazza/athena-console:panelkit -t ghcr.io/olivecasazza/athena-console:latest -
   docker push -q ghcr.io/olivecasazza/athena-console:panelkit; docker push -q ghcr.io/olivecasazza/athena-console:latest
   # operator
   docker load -q < $(nix build .#athena-operator-image --no-link --print-out-paths)
   docker tag ghcr.io/olivecasazza/athena-operator:dev ghcr.io/olivecasazza/athena-operator:latest
   docker push -q ghcr.io/olivecasazza/athena-operator:latest
   docker image inspect <image> --format '{{index .RepoDigests 0}}'
   ```
3. Pin the digests in nixlab from a **temporary worktree off `origin/main`**. Another session may own the main nixlab checkout on its own branch, so never rebase or stash it.
   ```bash
   cd ~/Repositories/nixlab && git fetch -q origin
   wt=$(mktemp -d) && git worktree add -q --detach $wt origin/main && cd $wt
   # edit modules/k8s/apps/athena-console.nix (panelkit@sha256:…) and/or athena.nix (latest@sha256:… # {"$imagepolicy": "apps:athena-operator"})
   cp $(nix build .#k8s-manifests --no-link --print-out-paths) modules/k8s/manifests.yaml; chmod u+w modules/k8s/manifests.yaml
   git add -A modules/k8s && git commit -m "feat(athena): …"   # commitizen types only: feat|fix|chore|…
   git push origin HEAD:main; cd - && git worktree remove --force $wt
   ```
   Pre-commit hooks do not run in the temp worktree, so always regenerate `manifests.yaml` yourself.
4. Nudge Flux and confirm. Image automation has not been reliably bumping the operator digest, so pin it explicitly.
   ```bash
   kubectl annotate gitrepository flux-system -n flux-system reconcile.fluxcd.io/requestedAt="$(date -Iseconds)" --overwrite
   kubectl annotate kustomization  flux-system -n flux-system reconcile.fluxcd.io/requestedAt="$(date -Iseconds)" --overwrite
   kubectl rollout status deploy/athena-console -n apps; kubectl rollout status deploy/athena -n apps
   ```

panel-kit (the console's UI kit) is the fork `olivecasazza/panel-kit`, pinned by rev in `operator/crates/athena-console-web/Cargo.toml`. Upstream is `ocasazza/panel-kit`. Its design rules are in `~/Repositories/panel-kit/.impeccable/design.json`: graphite surfaces, the accent only for live state, tracked caps for small labels, no shadows on resting surfaces.
