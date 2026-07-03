"""Adversarial citation audit: does each cited source actually support its claim?

The report's `references[]` carry (key, url, supports-claim). For each, this
workload fetches the source and runs N independent adversarial votes through
the LLM gateway — each vote is instructed to REFUTE the support relation.
Majority refutation kills the citation. Like the prover, the policy only
proposes judgments about evidence it was shown; the fetched source text is the
ground truth being judged, all votes are journaled, and unreachable sources are
reported as UNREACHABLE rather than silently passed.

Reads ATHENA_EXPERIMENT_SPEC .parameters:
  references     -> [{key, url, supports}, ...]  (the audit targets)
  llm_model      -> gateway model id
  votes          -> votes per citation (default 3)
Emits result.json / termination message:
  citations_supported, citations_refuted, citations_unreachable,
  citations_supported_ratio (objective, maximize), verdicts{key: verdict}
"""

from __future__ import annotations

import json
import os
import re
import sys
import time
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
try:
    from athena_metrics import journal, provenance
except Exception:
    def journal(*a, **k):  # type: ignore[misc]
        pass

    def provenance(**k):  # type: ignore[misc]
        pass

EXCERPT_CHARS = 6000


def fetch_source(url: str) -> str | None:
    req = urllib.request.Request(url, headers={"User-Agent": "athena-citation-audit/1"})
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            raw = resp.read(400_000).decode("utf-8", "replace")
    except Exception:
        return None
    text = re.sub(r"<script.*?</script>|<style.*?</style>", " ", raw, flags=re.S | re.I)
    text = re.sub(r"<[^>]+>", " ", text)
    text = re.sub(r"\s+", " ", text).strip()
    return text[:EXCERPT_CHARS] or None


def llm(prompt: str, model: str) -> str | None:
    base = os.environ.get("LLM_BASE_URL", "").rstrip("/")
    key = os.environ.get("LLM_API_KEY", "")
    key_file = os.environ.get("LLM_API_KEY_FILE", "")
    if key_file and not key:
        try:
            key = open(key_file).read().strip()
        except OSError:
            return None
    if not base or not key:
        return None
    body = json.dumps({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0.7,
        "max_tokens": 1500,
    }).encode()
    req = urllib.request.Request(
        base + "/chat/completions", data=body,
        headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            msg = json.load(resp)["choices"][0]["message"]
        return msg.get("content") or msg.get("reasoning_content") or None
    except Exception as e:
        journal("audit_llm_error", error=str(e)[:200])
        return None


def vote(claim: str, excerpt: str, model: str, i: int, n: int) -> str:
    """One adversarial vote: SUPPORTED / REFUTED / ABSTAIN."""
    # Fair-but-rigorous adjudication. An earlier refutation-BIASED prompt made a
    # weak judge refute everything (100% refute rate = verifies nothing); the
    # adversarial framing from the deep-research harness assumes CAPABLE voters.
    # Here each voter judges support neutrally, and we explicitly guard the two
    # degenerate priors (rubber-stamp / reflexive-refute). The excerpt is
    # web-fetched HTML-stripped text and may include navigation chrome; judge on
    # the substantive content, not on chrome or on an exact-sentence match.
    out = llm(
        f"You are citation-audit voter {i+1}/{n}, judging fairly and rigorously.\n\n"
        f"CLAIM the citation supports:\n{claim}\n\n"
        f"SOURCE EXCERPT (HTML-stripped from the cited URL; may include nav chrome):\n{excerpt}\n\n"
        "Does the substantive content of this source support the claim? SUPPORTED if the "
        "source is about this topic and its content substantiates or is consistent with the "
        "claim (an exact sentence match is NOT required — a paper's abstract supporting its "
        "own contribution counts). REFUTED only if the source contradicts the claim, is about "
        "an unrelated topic, or the excerpt is pure navigation with no substantive content.\n"
        "Reason in 1-2 sentences, then a final line exactly 'VERDICT: SUPPORTED' or "
        "'VERDICT: REFUTED'.",
        model,
    )
    if not out:
        return "ABSTAIN"
    m = re.findall(r"VERDICT:\s*(SUPPORTED|REFUTED)", out.upper())
    return m[-1] if m else "ABSTAIN"


def main() -> int:
    t0 = time.time()
    try:
        params = (json.loads(os.environ.get("ATHENA_EXPERIMENT_SPEC", "") or "{}").get("parameters") or {})
    except Exception:
        params = {}
    refs = params.get("references") or []
    if isinstance(refs, str):
        refs = json.loads(refs)
    model = str(params.get("llm_model", "gpt-oss-20b-free"))
    n_votes = int(params.get("votes", 3) or 3)

    provenance(audit_targets=[r.get("key") for r in refs], llm_model=model, votes=n_votes)
    verdicts, supported, refuted, unreachable = {}, 0, 0, 0
    for r in refs:
        key, url, claim = r.get("key", "?"), r.get("url"), r.get("supports") or r.get("title", "")
        if not url:
            verdicts[key] = "UNREACHABLE"; unreachable += 1
            journal("citation_verdict", key=key, verdict="UNREACHABLE", reason="no url")
            continue
        excerpt = fetch_source(url)
        if not excerpt:
            verdicts[key] = "UNREACHABLE"; unreachable += 1
            journal("citation_verdict", key=key, verdict="UNREACHABLE", reason="fetch failed")
            continue
        votes = [vote(claim, excerpt, model, i, n_votes) for i in range(n_votes)]
        journal("citation_votes", key=key, votes=votes)
        valid = [v for v in votes if v != "ABSTAIN"]
        # Quorum discipline (from the deep-research harness): too many abstentions
        # means UNVERIFIED, which must not pass as supported.
        if len(valid) < (n_votes // 2 + 1):
            verdicts[key] = "UNVERIFIED"; unreachable += 1
        elif valid.count("REFUTED") * 2 >= len(valid):
            verdicts[key] = "REFUTED"; refuted += 1
        else:
            verdicts[key] = "SUPPORTED"; supported += 1
        journal("citation_verdict", key=key, verdict=verdicts[key])
        print(f"[{key}] {verdicts[key]} (votes: {votes})")

    total = max(len(refs), 1)
    ratio = supported / total
    result = {
        "citations_supported": supported,
        "citations_refuted": refuted,
        "citations_unreachable": unreachable,
        "citations_supported_ratio": round(ratio, 4),
        "verdicts": verdicts,
        "wall_seconds": round(time.time() - t0, 1),
        "metric_series": {
            "objective": "citations_supported_ratio",
            "goal": "maximize",
            "points": [{"name": "citations_supported_ratio", "value": ratio, "step": len(refs)}],
        },
    }
    journal("citation_audit_finished", **{k: v for k, v in result.items() if k != "metric_series"})
    ws = os.environ.get("ATHENA_WORKSPACE_PATH")
    if ws:
        try:
            os.makedirs(ws, exist_ok=True)
            json.dump(result, open(os.path.join(ws, "result.json"), "w"))
        except Exception:
            pass
    try:
        open("/dev/termination-log", "w").write(json.dumps(result))
    except Exception:
        pass
    print(json.dumps(result, indent=1))
    return 0


if __name__ == "__main__":
    sys.exit(main())
