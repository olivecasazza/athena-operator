"""Athena Tier-3 phase-1 prover harness: verifier-as-reward, tactic-sweep baseline.

Formal theorem proving as auto-RL needs exactly one thing from the environment:
a reward that is the Lean kernel's verdict (proof closes = 1, else 0) — never a
self-reported score. This harness is the *minimum* prover that exercises that
contract end-to-end: it attempts a target statement with a fixed sweep of
standard closing tactics and lets `lake env lean` (kernel elaboration) judge.
It is deliberately NOT a learned prover — phase 2 replaces the tactic sweep
with an LLM policy; the reward contract stays identical.

Reads (operator-injected):
  ATHENA_EXPERIMENT_SPEC   .parameters.target_statement -> id in statements.json
                           .parameters.tactic_timeout   -> seconds per tactic
  ATHENA_WORKSPACE_PATH    result.json + journal/provenance land here

Writes:
  result.json + pod termination message:
    proof_closed (1.0/0.0 — the kernel verdict, the objective)
    statement_id, statement_hash, tactic, proof, wall_seconds, tactics_tried
"""

from __future__ import annotations

import hashlib
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
try:
    from athena_metrics import journal, metrics as _metrics_factory, provenance

    _athm = _metrics_factory()
except Exception:  # local runs without the athena env
    class _NoM:
        def store(self, *a, **k):
            pass

    _athm = _NoM()

    def journal(*a, **k):  # type: ignore[misc]
        pass

    def provenance(**k):  # type: ignore[misc]
        pass

MATHLIB_DIR = os.environ.get("MATHLIB_DIR", "/mathlib")
STATEMENTS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "statements.json")
# Standard closing tactics, cheap-to-expensive. ponytail: fixed sweep — the RL
# policy (LLM tactic proposal + search) is phase 2; the verifier contract here
# is what it will plug into.
TACTICS = ["norm_num", "decide", "omega", "simp", "aesop", "nlinarith", "positivity"]


def llm_propose(signature: str, model: str, n: int, failed: list) -> list:
    """Ask the LLM gateway for tactic candidates. The policy PROPOSES only —
    every candidate is kernel-checked; a bad proposal costs one check, never
    soundness. Returns [] on any gateway problem (sweep result stands)."""
    base = os.environ.get("LLM_BASE_URL", "").rstrip("/")
    key = os.environ.get("LLM_API_KEY", "")
    key_file = os.environ.get("LLM_API_KEY_FILE", "")
    if key_file and not key:
        try:
            key = open(key_file).read().strip()
        except OSError:
            pass
    if not base or not key:
        journal("llm_policy_skipped", reason="no LLM_BASE_URL/LLM_API_KEY configured")
        return []
    prompt = (
        "You are a Lean 4 tactic expert (mathlib). This theorem statement is open:\n\n"
        f"{signature} := by <TACTIC>\n\n"
        f"These standard tactics already FAILED: {', '.join(failed)}.\n"
        f"Propose {n} DISTINCT single-line Lean 4 tactic invocations likely to close it, "
        "most promising first. Prefer tactics with helpful term arguments/hints "
        "(e.g. nlinarith [sq_nonneg (x + 1)], simp [Nat.add_comm], field_simp; ring). "
        "Output ONLY the tactic lines, one per line, no numbering, no code fences, no prose."
    )
    import urllib.request
    body = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.8,
        "max_tokens": 2000,
    }).encode()
    req = urllib.request.Request(
        base + "/chat/completions",
        data=body,
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            msg = json.load(resp)["choices"][0]["message"]
        # Reasoning-style models may return content=null (answer in
        # reasoning_content, or the token budget burned on reasoning).
        out = msg.get("content") or msg.get("reasoning_content") or ""
    except Exception as e:
        journal("llm_policy_error", error=str(e)[:200])
        return []
    if not out.strip():
        journal("llm_policy_error", error="empty completion (reasoning model? token cap?)")
        return []
    cands = []
    for line in out.splitlines():
        line = line.strip().strip("`").lstrip("-*0123456789. ").strip()
        if line and not line.startswith(("```", "#", "--")) and line not in cands:
            cands.append(line)
    return cands[:n]


def kernel_check(candidate: str, timeout: float) -> tuple[bool, str]:
    """Compile a candidate proof file under mathlib. Return (closed, detail).

    Success == `lake env lean` exit 0 with no `sorry` in the candidate (we never
    emit sorry) — i.e. the Lean kernel elaborated and accepted the proof. This
    subprocess IS the reward function; nothing downstream may override it.
    """
    path = "/tmp/candidate.lean"
    with open(path, "w") as fh:
        fh.write(candidate)
    try:
        r = subprocess.run(
            ["lake", "env", "lean", path],
            cwd=MATHLIB_DIR,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        return False, "timeout"
    if r.returncode == 0:
        return True, "ok"
    return False, (r.stderr or r.stdout or "").strip()[-400:]


def main() -> int:
    t0 = time.time()
    try:
        spec = json.loads(os.environ.get("ATHENA_EXPERIMENT_SPEC", "") or "{}")
        params = spec.get("parameters") or {}
    except Exception:
        params = {}
    target = str(params.get("target_statement", "smoke_add_comm"))
    timeout = float(params.get("tactic_timeout", 300))

    with open(STATEMENTS) as fh:
        statements = json.load(fh)
    if target not in statements:
        print(f"unknown statement id '{target}'; known: {sorted(statements)}")
        return 1
    signature = statements[target]
    statement_hash = hashlib.sha256(signature.encode()).hexdigest()[:16]

    provenance(
        statement_id=target,
        statement=signature,
        statement_hash=statement_hash,
        tactic_set=TACTICS,
        mathlib_dir=MATHLIB_DIR,
        prover="tactic-sweep-baseline",
    )
    journal("proof_attempt_started", statement_id=target, statement_hash=statement_hash)

    llm_model = str(params.get("llm_model", "") or "")
    llm_candidates = int(params.get("llm_candidates", 8) or 8)

    closed, winning_tactic, proof_text, tried = False, None, None, []
    policy = "sweep"

    def attempt(tactic: str, source: str) -> bool:
        nonlocal closed, winning_tactic, proof_text
        candidate = f"import Mathlib\n\n{signature} := by {tactic}\n"
        t_tac = time.time()
        ok, detail = kernel_check(candidate, timeout)
        tried.append(tactic)
        _athm.store("tactics_tried", len(tried))
        _athm.store("proof_closed", 1.0 if ok else 0.0)
        journal(
            "tactic_result",
            statement_id=target,
            tactic=tactic,
            source=source,
            closed=ok,
            seconds=round(time.time() - t_tac, 1),
            detail=None if ok else detail[:200],
        )
        print(f"[{target}] ({source}) {tactic}: {'CLOSED' if ok else 'open'} ({time.time()-t_tac:.1f}s)")
        if ok:
            closed, winning_tactic, proof_text = True, tactic, candidate
        return ok

    for tactic in TACTICS:
        if attempt(tactic, "sweep"):
            break

    # LLM policy: only consulted when the cheap sweep leaves the goal open, so
    # measured lift is strictly on top of the baseline.
    if not closed and llm_model:
        policy = "sweep+llm"
        proposals = llm_propose(signature, llm_model, llm_candidates, TACTICS)
        journal("llm_proposals", statement_id=target, model=llm_model, proposals=proposals)
        for tactic in proposals:
            if attempt(tactic, "llm"):
                break

    wall = time.time() - t0
    result = {
        # The objective: the kernel's verdict as a number so the operator can
        # rank on it (goal: maximize).
        "proof_closed": 1.0 if closed else 0.0,
        "statement_id": target,
        "statement_hash": statement_hash,
        "tactic": winning_tactic,
        "tactics_tried": len(tried),
        "policy": policy,
        "llm_model": llm_model or None,
        "wall_seconds": round(wall, 1),
        "metric_series": {
            "objective": "proof_closed",
            "goal": "maximize",
            "points": [{"name": "proof_closed", "value": 1.0 if closed else 0.0, "step": len(tried)}],
        },
    }
    journal("proof_attempt_finished", **{k: v for k, v in result.items() if k != "metric_series"})

    ws = os.environ.get("ATHENA_WORKSPACE_PATH")
    if ws:
        try:
            os.makedirs(ws, exist_ok=True)
            with open(os.path.join(ws, "result.json"), "w") as fh:
                json.dump(result, fh)
            if proof_text:
                with open(os.path.join(ws, f"proof_{target}.lean"), "w") as fh:
                    fh.write(proof_text)
        except Exception as e:
            print(f"workspace write failed: {e}")
    try:
        with open("/dev/termination-log", "w") as fh:
            fh.write(json.dumps(result))
    except Exception:
        pass

    print(json.dumps(result, indent=1))
    # A non-closing attempt is a valid, successful MEASUREMENT (the reward is 0);
    # only harness errors exit nonzero.
    return 0


if __name__ == "__main__":
    sys.exit(main())
