# Kueue stuck-workload runbook

Scope: GPU workloads submitted by this operator that never start, and GPU workloads
any other producer leaves pending in the same queues.

All source citations are `repo:file:line`. Repos referenced:

| Short name | Path on `seir` | Branch at time of writing |
|---|---|---|
| **athena** | `/home/olive/Repositories/athena-operator` | `main` |
| **nixlab** | `/home/olive/Repositories/nixlab` | `fix/ha-rollout-on-config-change` |

Facts are marked **[V]** (verified against source or the live cluster on 2026-09-28)
or **[I]** (inference). No build or test was run to produce this document.

---

## 1. TL;DR

> **Every GPU flavor in `hp-gpu` and `kepler-gpu` is Topology-Aware-Scheduling-only, and this
> operator cannot emit a TAS annotation.** Not one — there is no field in the CRD and no code
> path that produces one. A GPU job submitted through `athena-gpu` is therefore *structurally
> unadmittable*, and it fails silently: no pod event, no crash, no log line. It just sits.

Verified live message, taken from a pending `ResearchCampaign` experiment Job:

```
couldn't assign flavors to pod set main: Flavor "rtx4000" supports only
TopologyAwareScheduling, Flavor "rtx5000" supports only TopologyAwareScheduling,
insufficient quota for nvidia.com/gpu in flavor cpu-any, previously considered
podsets requests (0) + current podset request (1) > maximum capacity (0)
```

`hp-gpu` had **17 pending** and **1 admitted** workload at the time of writing. **[V]**

---

## 2. Ownership: which repo decides what

| Thing | Owned by | Where |
|---|---|---|
| `ClusterQueue` / `ResourceFlavor` / `Topology` / `LocalQueue` | **nixlab** | `nixlab:modules/k8s/kueue/queues.nix` (641 lines, the single source) |
| TAS annotation on a pod template | **nixlab** (hand-written per workload) | e.g. `nixlab:modules/k8s/apps/north-mini-code.nix:123` |
| Operator CRD schema | **athena** | `athena:charts/athena/crds/athena-crds.yaml` (only CRD copy in the repo) |
| Operator pod/Job/RayJob builders | **athena** | `athena:operator/crates/athena/src/` |
| Whether a pod asks for topology | **neither** — see §3 | — |

The operator is a *Kueue client only*. It never reads or writes a `ClusterQueue`. Its entire
Kueue surface is two labels, stamped on the pod template or Job metadata:
`kueue.x-k8s.io/queue-name` and `kueue.x-k8s.io/priority-class`
(`athena:operator/crates/athena/src/reconciler.rs:1041-1043`,
`athena:operator/crates/athena/src/campaign_reconciler.rs:2169-2172`,
`athena:operator/crates/athena/src/benchmark_reconciler.rs:340`). **[V]**

`athena:modules/k8s/manifests.yaml` contains **zero** Kueue objects (grep for `kueue` returns
nothing) and is not applied by Flux — Flux applies nixlab. Do not edit it for scheduling
changes. **[V]**

---

## 3. The TAS finding

### 3.1 What the assignment asked to verify

**Claim: no TAS fields appear anywhere in the CRD schema or the pod/job builders.**
**Result: VERIFIED TRUE — zero occurrences.**

Exhaustive case-sensitive grep across all 190 non-vendored `*.rs`, `*.yaml`, `*.yml`, `*.nix`,
`*.py`, `*.ts`, `*.md` files in the athena repo (excluding `.git`, `.venv`, `.direnv`, `result`,
`.jj`, `.claude`, `.slim`, `.opencode`, caches) for:

```
topologySpreadConstraints  TopologySpreadConstraint  requiredDuringSchedulingIgnoredDuringExecution
minDomains  whenUnsatisfiable  nodeTaintsPolicy  nodeAffinityPolicy  maxSkew
podset-preferred-topology  podset-required-topology  TopologyAwareScheduling  gpu-topology
```

→ **exit status 1, no matches.** A second case-insensitive pass over all 138 git-tracked files for
`affinity|topologySpread|minDomains|whenUnsatisfiable` returned only unrelated hits: a prose
sentence in `athena:docs/openspec.md:1100` and the string `binding_affinity` used as a Ray
reward name in `athena:examples/auto-rl/auto-rl-drug-discovery.yaml:37,43,44,56,58`. No
Kubernetes scheduling field, in any file. **[V]**

### 3.2 The CRD schema, positively

`RuntimeProfile.spec.scheduling` is the complete set of knobs a user has
(`athena:charts/athena/crds/athena-crds.yaml:216-250`):

| Field | Line |
|---|---|
| `experimentDeadline` | `athena-crds.yaml:219` |
| `nodeSelector` | `athena-crds.yaml:225` |
| `priorityClassName` | `athena-crds.yaml:231` |
| `queueName` | `athena-crds.yaml:235` |
| `runtimeClassName` | `athena-crds.yaml:240` |
| `tolerations` | `athena-crds.yaml:245` |

A grep for `"affinity"`, `"topologySpreadConstraints"`, `"minDomains"`,
`"requiredDuringScheduling"`, `"podAntiAffinity"`, `"nodeAffinity"` against the whole 163 KB CRD
file returns **0**. **[V]** The same is true of the two campaign kinds —
`VllmClusterSpec` (`athena:operator/crates/athena-api/src/research_campaign.rs:142-176`) and
`InferenceMeshSpec` (`research_campaign.rs:214-245`), whose `queueName` schemas are at
`athena-crds.yaml:797` and `athena-crds.yaml:869`. **[V]**

The Rust source matches: `SchedulingProfile` (`athena:operator/crates/athena-api/src/runtime_profile.rs:275-307`)
has exactly `node_selector`, `tolerations`, `priority_class_name`, `runtime_class_name`,
`queue_name`. **[V]**

The builders copy those fields and nothing more — no annotation, no `affinity`:
`reconciler.rs:1391-1408` (Job pod spec), `campaign_reconciler.rs:2146-2151` and `:2195-2196` and
`:2223-2224` (RayJob head + worker pod specs), `campaign_reconciler.rs:1869-1878` (mesh pod
labels), `benchmark_reconciler.rs:421-433`. **[V]**

### 3.3 What the flavors require

All five GPU flavors in nixlab carry `topologyName = "gpu-topology"`, which makes them
TAS-only in Kueue's eyes: **[V]**

| Flavor | Line | `topologyName` line | Node selector |
|---|---|---|---|
| `rtx4000` | `nixlab:queues.nix:96` | `queues.nix:104` | `nvidia.com/gpu.product=Quadro-RTX-4000` (`:103`) |
| `rtx5000` | `queues.nix:128` | `queues.nix:131` | `nvidia.com/gpu.product=Quadro-RTX-5000` (`:130`) |
| `kepler` | `queues.nix:179` | `queues.nix:182` | `kubernetes.io/hostname=tyan01` (`:181`) |
| `ada` | `queues.nix:206` | `queues.nix:209` | `kubernetes.io/hostname=contra` (`:208`) |
| `amd` | `queues.nix:224` | `queues.nix:227` | `kubernetes.io/hostname=traitor` (`:226`) |

`cpu-any` is the sole exception — its `spec` is **omitted entirely**
(`nixlab:queues.nix:244-248`), so it has no topology, no node labels, no tolerations. Live
confirmation: `kubectl get resourceflavor cpu-any -o json` → `{"spec":{}}`. **[V]**

The topology itself (`nixlab:queues.nix:83-92`) is three levels, widest to narrowest:
`topology.kubernetes.io/zone` → `nixlab.io/rack` → `kubernetes.io/hostname`. It was created
2026-09-28T01:49:07Z, i.e. ~25 h before this document, by commit `4294d509`
*"feat(kueue): topology-aware scheduling so TP groups stay on one machine"*. **[V]**

### 3.4 The trap, stated exactly

nixlab already knows about this. `nixlab:modules/k8s/apps/north-mini-code.nix:114-123`:

> `# REQUIRED, not an optimisation. Every GPU ResourceFlavor carries`
> `# topologyName = "gpu-topology", and a TAS flavor admits only`
> `# podsets that request topology. Without this the pod sits`
> `# SchedulingGated forever behind`
> `#   Flavor "rtx5000" supports only TopologyAwareScheduling`

and `nixlab:modules/k8s/apps/tei/default.nix:46-59` records the identical break already biting a
non-operator workload on 2026-09-28, with the note that any *other* pre-existing workload on the
kepler queues *"has the same latent break and will fail the next time it is rescheduled."* **[V]**

Only three hand-written nixlab workloads carry the annotation:
`north-mini-code.nix:123`, `tei/default.nix:65`, `tei/amd.nix:66`. Everything else in the fleet —
including every GPU workload this operator creates — does not. **[V]**

---

## 4. Flavor and quota table for `hp-gpu`

`hp-gpu` is defined at `nixlab:queues.nix:249-356`. Cohort `gpu` (`:275`), preemption
`withinClusterQueue/reclaimWithinCohort/borrowWithinCohort = LowerPriority` (`:276-280`).
Covered resources: `cpu`, `memory`, `nvidia.com/gpu` (`:283-287`). **[V]**

Flavor order is the search order, and it is deliberate (`queues.nix:290-291`):
`cpu-any` is tried first and is the **trap** for GPU podsets.

| # | Flavor | `cpu` | `memory` | `nvidia.com/gpu` | TAS? | Node labels injected | Definition |
|---|---|---|---|---|---|---|---|
| 1 | `cpu-any` | `16` | `64Gi` | **`0`** | **no** (empty spec) | none | `queues.nix:292-307`, quota `:296/:300/:304`; flavor `:244-248` |
| 2 | `rtx4000` | `48` | `192Gi` | `3` | **yes** | `Quadro-RTX-4000` | `queues.nix:309-324`, quota `:313/:317/:321`; flavor `:93-118` |
| 3 | `rtx5000` | `6` | `16Gi` | `2` | **yes** | `Quadro-RTX-5000` | `queues.nix:336-351`, quota `:340/:344/:348`; flavor `:125-133` |

**`cpu-any` has an explicit `nvidia.com/gpu: nominalQuota = "0"`** — the literal string `"0"` at
`nixlab:queues.nix:302-305`, not an absent key and not `lendingLimit`/`borrowingLimit`. Live
`kubectl get clusterqueue hp-gpu -o jsonpath='{.spec.resourceGroups}'` matches the Nix source
field for field. It is the *only* `nvidia.com/gpu` line in that flavor. The same
`nominalQuota = "0"` pattern is repeated in `kepler-gpu` at `nixlab:queues.nix:449-462`. **[V]**

So the two facts compose into a hard deadlock for a GPU podset:

1. `cpu-any` is tried first and is the only flavor this operator can reach — it needs no topology.
2. It has **zero** GPU quota, so a podset requesting `nvidia.com/gpu: 1` cannot fit.
3. The fall-through targets `rtx4000` / `rtx5000` both reject non-TAS podsets.
4. No flavor remains. Kueue reports all three failures in one message and the workload waits.

The `cpu-any` flavor exists for a good reason and must not be removed: it was added because a
RayJob's CPU-only *head* pod, once given a GPU nodeSelector by the `rtx4000` flavor, had no
`nvidia.com/gpu` taint toleration and deadlocked unschedulably
(`nixlab:queues.nix:237-243`). It is a head-landing lane, not GPU capacity. **[V]**

### 4.1 Live flavor usage at time of writing

`kubectl describe clusterqueue hp-gpu` → `Flavors Reservation` (`total` = in use): **[V]**

| Flavor | cpu | memory | nvidia.com/gpu |
|---|---|---|---|
| `cpu-any` | 0 | 0 | 0 |
| `rtx4000` | 0 | 0 | **0** |
| `rtx5000` | 1 | 4Gi | **2** (of 2) |

`rtx4000`'s full 3-GPU quota is **sitting idle** while 17 workloads wait. Physical confirmation:
`hp01`, `hp02`, `hp03` are all `Ready`, each with `nvidia.com/gpu` allocatable `1` and the
`Quadro-RTX-4000` product label. The nodes are not the constraint. The annotation is. **[V]**

The single admitted workload is `apps/pod-north-mini-code-…` on `rtx5000`, requesting both seir
GPUs, and it carries the `topologyAssignment` block (`kubernetes.io/hostname` → `seir`) — i.e. the
one working GPU workload in the queue is the one that carries the annotation. **[V]**

> **Side note, harmless but confusing:** `rtx4000` injects a toleration for the
> `nixlab.io/gpu-kueue` taint (`nixlab:queues.nix:112-115`, described at `:97-98` as set by the
> hp hardware module). No hp node currently carries that taint — `hp01/02/03` show taints
> `nvidia.com/gpu` only. A toleration for an absent taint is a no-op, so this is not a defect
> today; it is a stale comment. **[V]**

---

### 4.2 The large "admitted" counts are finished history, not live demand

Counting `Workload` objects by admission state alone reads as a healthy queue and is
misleading. Kueue keeps a `Workload` object after its job completes; those objects stay
`Admitted` forever. Measured across the `athena-gpu` and `athena-kepler` queues:

| Queue | Admission | Cycle | Count | Age (oldest / median / youngest) |
|---|---|---|---|---|
| `athena-gpu` | Admitted | **Finished** | 227 | 1778 h / 734 h / 51 h |
| `athena-gpu` | not admitted | **live** | 17 | 51 h / 13 h / 7 h |
| `athena-kepler` | Admitted | **Finished** | 14 | — |

The split is total: **every** admitted `athena-*` workload carries a `Finished` condition,
and **every** non-admitted one is live. The youngest admitted workload is older than
`4294d509` (§3.3), which is the signature of the regression: nothing has been admitted since
topology-aware scheduling landed.

So "227 admitted / 17 pending" is not 93 % success — it is 227 historical GPU jobs that ran
before the change, and zero that have run since. GPU demand through athena has been at exactly
zero throughput for the ~25 h since the topology was introduced. **[V]**

Two consequences for triage:

- A high admitted count is **not** evidence that a queue is working. Check for a `Finished`
  condition, not for `Admitted`.
- When counting queue depth, filter on the absence of `Finished`; counting `status.admission`
  or raw `Admitted` counts terminal history.

The one workload still holding GPU quota under `hp-gpu` is not an athena submission: it is the
hand-written `north-mini-code` Job, which carries the required topology annotation
(`nixlab:modules/k8s/apps/north-mini-code.nix:114-123`). That is why `rtx4000`'s quota reads
`total=0` while the queue reports 17 pending — and it is the direct evidence that the
annotation, not the GPU hardware, is what is missing. **[V]**

## 5. Second, distinct failure: `no topology domains at level`

Do not confuse this with §3. One pending workload, `apps/pod-tei-6c5867d4f8-xq6z7-ac306`, reports:

```
couldn't assign flavors to pod set main: no topology domains at level: kubernetes.io/hostname
```

Here the annotation **is** present (`nixlab:modules/k8s/apps/tei/default.nix:65`) and the flavor
is `kepler` (pinned to `kubernetes.io/hostname=tyan01`, `queues.nix:181`). The reason there are
no domains is that `tyan01` is `NotReady` and `spec.unschedulable=true`, with
`node.kubernetes.io/unreachable` taints. TAS computes its domain set from *Ready* nodes, so the
hostname level is empty. **[V]**

This is a node-health problem, not an annotation problem. Fix the node, not the manifest. The
`kepler` flavor also under-reports its true capacity: its comment (`queues.nix:159-163`) records
GPU 0 as dead with repeating Xid 122, and `tyan01.status.allocatable["nvidia.com/gpu"]` is `0`
right now. **[V]**

---

## 6. Triage

Work top to bottom; stop at the first match.

```bash
export KUBECONFIG=$HOME/.kube/config        # run on seir
```

**Step 0 — is it actually stuck?** Use `kubectl get clusterqueue`, not `kubectl get workload`.

```bash
kubectl get clusterqueue
```

> **Do not trust `kubectl get workload` for this.** At time of writing 253 Workload objects in
> namespace `apps` carried a populated `.status.admission`, and 250 of them already had a
> `Finished` condition — Kueue never clears `status.admission`, so the object count reads like a
> busy, healthy queue. Across all namespaces: 262 with a populated admission, 3 without a
> `Finished` condition. Filter on conditions, never on `.status.admission != null`:
> ```bash
> kubectl get workload -A -o json | jq -r '
>   .items[] | select([.status.conditions[]?|select(.type=="Finished")]|length==0)
>   | select(.status.admission==null) | "\(.metadata.namespace)/\(.metadata.name)"'
> ```
> At time of writing this returned 18 workloads, all in namespace `apps`: 17 submitted through
> LocalQueue `athena-gpu` (→ `hp-gpu`, §3) and 1 through `apps-kepler` (→ `kepler-gpu`, the
> `tei` pod of §5). **[V]**

**Step 1 — read the Kueue message.** It names the flavor, so it tells you which branch applies.

```bash
kubectl get workload -A -o json | jq -r '.items[]
  | select([.status.conditions[]?|select(.type=="Finished")]|length==0)
  | select(.status.admission==null)
  | .metadata.name as $n
  | (.status.conditions[]?|select(.type=="QuotaReserved" and .status=="False")|.message)
  | "\($n)\n  \(.)"'
```

**Step 2 — branch on the message.**

| Message fragment | Cause | Go to |
|---|---|---|
| `supports only TopologyAwareScheduling` | No TAS annotation on the podset. **This operator always produces this.** | §7.1 |
| `no topology domains at level: X` | The flavor's node(s) are NotReady / unschedulable | §7.2 |
| `insufficient quota for R in flavor F` *without* a TAS complaint | Genuine exhaustion. Check `flavorsUsage`. | §7.3 |
| `Previously: Preempted to accommodate a workload` | Transient; re-check after the evicting job lands | §7.3 |

**Step 3 — confirm capacity is actually free.** If `total` for the flavor is less than its
`nominalQuota`, it is a quota problem, not a TAS problem, and §7.1 will not help.

```bash
kubectl describe clusterqueue hp-gpu | sed -n '/Flavors Reservation/,/^$/p'
```

---

## 7. Remediation, ordered by blast radius

Per `nixlab`'s and this repo's own policy: writes are modeled in Git and applied by Flux, not by
hand (`athena:AGENTS.md:181`). **Nothing below is a live `kubectl edit`.**

### 7.1 Give the podset a topology request

Three options, in increasing order of durability.

**(a) Immediate unblock, no code change — annotate the pod template in nixlab.**
Copy the pattern from `nixlab:modules/k8s/apps/north-mini-code.nix:123`:

```nix
"kueue.x-k8s.io/podset-preferred-topology" = "kubernetes.io/hostname";
```

Use `podset-preferred-topology`, not `podset-required-topology`: nixlab's own reasoning at
`north-mini-code.nix:119-122` and `tei/default.nix:61-64` is that `preferred` degrades to
"schedule anyway" if labels are incomplete, whereas `required` strands the workload. It also notes
TAS only binds multi-pod podsets; a single pod asking for `nvidia.com/gpu: 2` is already confined
to one node (`queues.nix:78-82`). **[V]**

*Limitation:* this only works for workloads whose pod template you can reach in nixlab. It does
**not** help anything this operator builds — see (b).

**(b) The real fix — give the operator a TAS field.** This is a change to **athena**, not nixlab.
It has four coordinated parts, and all four are required:

1. Add the field to `SchedulingProfile` (`athena:operator/crates/athena-api/src/runtime_profile.rs:277`).
2. Same for `VllmClusterSpec` (`research_campaign.rs:142`) and `InferenceMeshSpec`
   (`research_campaign.rs:214`).
3. Stamp it onto the pod template in **all four** builders: `reconciler.rs:1391-1408`,
   `campaign_reconciler.rs:2195-2196` and `:2223-2224` and `:1869-1878`,
   `benchmark_reconciler.rs:421-433`.
4. Regenerate `charts/athena/crds/athena-crds.yaml`. It is emitted by
   `athena:operator/crates/athena/src/crd.rs:12-29` (`RuntimeProfile::crd()` etc.), so a schema
   change without regeneration leaves the apiserver silently pruning the field.

Prefer a single enum with a default that matches the fleet — e.g.
`scheduling.topology: kubernetes.io/hostname` as the default — over a raw free-text string, so
the common case needs no user action and the failure mode cannot recur through omission. **[I]**

*Read the blast radius before choosing:* adding a default topology request to GPU profiles changes
placement for **every already-admitted-then-rescheduled** GPU workload in the estate at once —
exactly what happened to `tei` (`nixlab:modules/k8s/apps/tei/default.nix:53-59`). Any node in any
flavor that lacks one of the three topology labels becomes unplaceable for that workload. hp01-03
currently carry all three (`topology.kubernetes.io/zone=local`, `nixlab.io/rack=rack-1`,
`kubernetes.io/hostname`); `seir` carries `nixlab.io/workstation` as a *taint*, which is
unrelated, and is verified working via the live `topologyAssignment` on the `rtx5000` workload. **[V]**

**(c) Remove the TAS requirement from the flavors.** Deleting `topologyName` from `rtx4000` /
`rtx5000` (`nixlab:queues.nix:104`, `:131`) makes them non-TAS and unblocks this operator
immediately. **Do not do this to `rtx4000` without reading the reason it was added**
(`nixlab:queues.nix:54-82`): the topology exists specifically to stop a tensor-parallel group from
spanning hp01 and hp02 across a LAN with no GPU fabric, which is a silent multi-hour performance
collapse rather than a clean failure. It is the right call for single-pod `nvidia.com/gpu: 1`
workloads and the wrong call for TP>1. **[V]**

### 7.2 Node down / unschedulable

`no topology domains at level` means the flavor's node is not Ready. Check
`kubectl get nodes` and the `node.kubernetes.io/unreachable` / `cilium agent-not-ready` taints.
Restore the node. Do **not** add the annotation — `tei` already has it
(`nixlab:modules/k8s/apps/tei/default.nix:65`) and still fails, because the domain genuinely does
not exist. **[V]**

### 7.3 Genuine exhaustion

`hp-gpu` runs preemption rather than numeric caps — borrowing/lending limits are deliberately
unset so idle GPUs cross pool boundaries (`nixlab:queues.nix:272-274`). All four GPU
ClusterQueues share one `gpu` cohort (`queues.nix:275`), so a pending workload can preempt
lower-priority borrowers elsewhere. The levers, in order of preference:

1. **Wait for preemption** — check `kueue.x-k8s.io/priority-class`. `mesh-high` (10000) reclaims
   from default-priority spot research (`nixlab:queues.nix:258-260`).
2. **Raise the priority** of the blocked workload, if it is genuinely serving work.
3. **Raise the flavor's `nominalQuota`** in `nixlab:modules/k8s/kueue/queues.nix` — but check the
   node can physically deliver it first. The `rtx5000` quota was cut from 10 cpu / 36Gi to
   6 cpu / 16Gi for exactly this reason (`queues.nix:326-335`): seir is a workstation and
   promising capacity it cannot deliver caused workloads to admit and then fail.

---

## 8. Pre-merge rules

Checkable. Each maps to a defect that has actually occurred.

**R1 — Any change to a `ResourceFlavor` must re-read the `hp-gpu` flavor *order*, not just the
quota.** Adding `topologyName` to a flavor (`nixlab:queues.nix:104`, `:131`, `:182`, `:209`,
`:227`) silently removes it from the search space for every workload that does not request
topology. Grep the diff for `topologyName` and treat any addition as a breaking change requiring
an inventory of affected producers. This is the exact regression of `4294d509`.

**R2 — Any change to a `ResourceFlavor` must list, by name, every workload that will be affected.**
The three annotated nixlab workloads are `north-mini-code` (`north-mini-code.nix:123`), `tei`
(`tei/default.nix:65`), `tei-amd` (`tei/amd.nix:66`). There is no fourth. If the change would
break one, the diff is incomplete.

**R3 — `cpu-any` must keep `nvidia.com/gpu: nominalQuota = "0"`** (`nixlab:queues.nix:302-305`,
and `:461` in `kepler-gpu`). It is the deliberate fall-through switch, not an oversight. Raising
it makes CPU-only podsets grab GPU quota and starves real GPU work. Deleting the flavor
reintroduces the Ray-head unschedulable deadlock documented at `queues.nix:237-243`.

**R4 — An operator change that touches scheduling must update the CRD YAML in the same commit.**
`charts/athena/crds/athena-crds.yaml` is generated by `operator/crates/athena/src/crd.rs:12-29`.
A Rust field with no regenerated schema is silently dropped by the apiserver. The schema is
structural, not `x-kubernetes-preserve-unknown-fields` — see the note at
`athena-crds.yaml:879-880` about an array-typed `tolerations` failing an install outright.

**R5 — Before claiming a GPU workload "works", confirm it is in the `Flavors Reservation` table
of a live `kubectl describe clusterqueue`, with a `topologyAssignment` block if the flavor is
TAS.** A pod template that builds, installs and runs a container is not evidence of admission.
`pod-north-mini-code` is the only admitted GPU workload in `hp-gpu` and it is the only one with
`topologyAssignment` — the correlation is exact. **[V]**

**R6 — Do not count Workload objects as a health metric.** 250 of 253 stale objects in namespace
`apps` had a populated `.status.admission` and a `Finished` condition, which made `hp-gpu` look
saturated. Use `kubectl get clusterqueue` `PENDING WORKLOADS`, or filter on the `Finished`
condition (§6 Step 0).

**R7 — A new `RuntimeProfile` requesting `nvidia.com/gpu` MUST set `scheduling.queueName`**
(`athena:AGENTS.md:188`, existing rule — restated here because it is the one control that is
actually enforced today, and the only reason this failure is *visible* rather than invisible).
Without it the Job bypasses Kueue entirely and grabs physical capacity.

**R8 — Do not hand-edit a pod template in a manifest that a chart or a generator owns.** The TAS
annotation currently has to be written into nixlab by hand, per workload
(`north-mini-code.nix:123`). That is a known wart, not a pattern to extend: it did not scale to
the operator, which is the whole of this incident. Fix it at the builder (§7.1b).

---

## 9. What this document does not cover

- **`nixlab` skill `gpu-scheduling`** (`olivecasazza/skills`) is cited by
  `nixlab:queues.nix:1` and `athena:AGENTS.md:186` as the canonical strategy. It was not read;
  if it contains a TAS section, it takes precedence over §3.
- **The `ada` and `amd` flavors** (`queues.nix:206-236`) are TAS-only by the same construction
  and carry the same latent break. No workload is pending there at time of writing, so no live
  evidence is presented for them. **[I]**
- **The imgen chart's `webui.gpuProduct = "Quadro-RTX-5000"` steering**
  (`nixlab:modules/k8s/apps/imgen/flux.nix:90-92`) relies on injected flavor labels to land on a
  specific product. Not exercised by any pending workload at time of writing. **[I]**
- **Build and test verification was not run**, per the assignment. The assertions here are
  source-grep and live-API reads only.
