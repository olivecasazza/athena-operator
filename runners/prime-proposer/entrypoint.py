"""Prime Agent research-proposer runner.

Contract (the drive controller is the only caller):

  env LLM_BASE_URL, LLM_MODEL, LLM_API_KEY   OpenAI-compatible model (OmniRoute)
  env ATHENA_MCP_URL                        Athena MCP endpoint (read-only tools)
  env PROPOSER_SYSTEM                       proposer instructions + JSON schema
  env PROPOSER_CONTEXT                      JSON context the controller built
  env MAX_TURNS, TIMEOUT_SECONDS            autonomy bounds

  out /dev/termination-log                  compact {"summary", "actions"} JSON
                                            (or {"error": ...} with exit 1)

The runner is stateless: its whole configuration lives under an emptyDir, it
holds no Kubernetes credentials, and the controller validates every action as
untrusted input.
"""

import json
import os
import subprocess
import sys
from pathlib import Path

TERMINATION_LOG = Path(os.environ.get("TERMINATION_LOG", "/dev/termination-log"))
# Kubernetes truncates termination messages at 4096 bytes.
MAX_MESSAGE = 4000
PROPOSAL = Path("/work/out/proposal.json")
# The proposer only needs to read: report writes stay with the drive's own
# write-up path and human curators.
READ_TOOLS = [
    "list_campaigns",
    "list_experiments",
    "list_reports",
    "list_drives",
    "get_report",
    "get_manifest",
    "get_scheduling",
]


def finish(payload: dict, code: int) -> None:
    text = json.dumps(payload, separators=(",", ":"))
    if len(text.encode()) > MAX_MESSAGE:
        text = json.dumps({"error": f"proposal is {len(text)} bytes; limit {MAX_MESSAGE}"})
        code = 1
    try:
        TERMINATION_LOG.write_text(text)
    except OSError:
        pass
    print(text)
    sys.exit(code)


def configure(home: Path) -> None:
    home.mkdir(parents=True, exist_ok=True)
    model = os.environ["LLM_MODEL"]
    (home / "models.json").write_text(
        json.dumps(
            {
                "providers": {
                    "athena": {
                        "baseUrl": os.environ["LLM_BASE_URL"].rstrip("/"),
                        "api": "openai-completions",
                        # Name of the env var, resolved by Prime Agent at request time.
                        "apiKey": "LLM_API_KEY",
                        "compat": {"supportsDeveloperRole": False, "supportsReasoningEffort": False},
                        "models": [{"id": model, "name": model, "contextWindow": 128000, "maxTokens": 8192}],
                    }
                }
            }
        )
    )
    (home / "settings.json").write_text(
        json.dumps(
            {
                "mcpServers": {
                    "athena": {
                        "type": "http",
                        "url": os.environ["ATHENA_MCP_URL"],
                        "enabledTools": READ_TOOLS,
                    }
                }
            }
        )
    )


def extract_json(text: str):
    text = text.strip()
    for fence in ("```json", "```"):
        if text.startswith(fence):
            text = text[len(fence):]
    text = text.rstrip("`").strip()
    start, end = text.find("{"), text.rfind("}")
    if start < 0 or end <= start:
        return None
    try:
        return json.loads(text[start : end + 1])
    except json.JSONDecodeError:
        return None


def last_assistant_text(events: str) -> str:
    text = ""
    for line in events.splitlines():
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        if ev.get("type") == "message_end" and ev.get("message", {}).get("role") == "assistant":
            parts = ev["message"].get("content") or []
            text = "".join(p.get("text", "") for p in parts if isinstance(p, dict) and p.get("type") == "text") or text
    return text


def stream(cmd: list, timeout: int):
    """Run prime-agent, logging each tool call and assistant turn to stderr
    (the pod log) as it happens, and return (events, returncode, stderr)."""
    import threading

    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    err_chunks: list = []
    threading.Thread(target=lambda: err_chunks.append(proc.stderr.read()), daemon=True).start()
    timer = threading.Timer(timeout, proc.kill)
    timer.start()
    lines = []
    for line in proc.stdout:
        lines.append(line)
        try:
            ev = json.loads(line)
        except json.JSONDecodeError:
            continue
        kind = ev.get("type")
        if kind == "tool_execution_start":
            code = json.dumps(ev.get("args", {}))[:300]
            print(f"[tool] {ev.get('toolName')} {code}", file=sys.stderr, flush=True)
        elif kind == "tool_execution_end" and ev.get("isError"):
            print(f"[tool-error] {json.dumps(ev.get('result'))[:300]}", file=sys.stderr, flush=True)
        elif kind == "message_end" and ev.get("message", {}).get("role") == "assistant":
            text = last_assistant_text(line)
            if text:
                print(f"[assistant] {text[:300]}", file=sys.stderr, flush=True)
    proc.wait()
    timer.cancel()
    if not timer.is_alive() and proc.returncode not in (0, None) and proc.returncode < 0:
        print(f"[runner] prime-agent killed after {timeout}s", file=sys.stderr, flush=True)
    return "".join(lines), proc.returncode, "".join(err_chunks)


def main() -> None:
    home = Path(os.environ.get("PRIME_AGENT_CODING_AGENT_DIR", "/work/.prime"))
    configure(home)
    PROPOSAL.parent.mkdir(parents=True, exist_ok=True)
    context_path = Path("/work/out/context.json")
    context_path.write_text(os.environ["PROPOSER_CONTEXT"])

    task = (
        os.environ["PROPOSER_SYSTEM"]
        + "\n\nYou are running headless inside a Kubernetes Job. The controller's context "
        f"for this decision is in {context_path} (read it with Python). The `mcp` module is "
        "pre-imported: `await mcp.list_tools(\"athena\")`, `await mcp.call_tool(\"athena\", "
        "<tool>, {...})` reads live Athena state (campaigns, experiments, reports with "
        "footguns). Use it to check prior art before proposing. When done, write the "
        f"proposal as STRICT JSON to {PROPOSAL} and reply with the same JSON. Keep it "
        f"under {MAX_MESSAGE} bytes: at most 4 actions, concise hypotheses. Your budget "
        f"is {os.environ.get('TIMEOUT_SECONDS', '900')} seconds and the run is killed at "
        "the limit: write a first valid proposal to that file early, then overwrite it "
        "as your evidence improves, so a killed run still delivers your best answer."
    )
    timeout = int(os.environ.get("TIMEOUT_SECONDS", "900"))
    cmd = [
        "prime-agent",
        "-p",
        "--no-session",
        "--offline",
        "--mode",
        "json",
        "--model",
        f"athena/{os.environ['LLM_MODEL']}",
        "--autonomous",
        "--autonomous-max-turns",
        os.environ.get("MAX_TURNS", "16"),
        "--autonomous-timeout-ms",
        str(timeout * 1000),
        task,
    ]
    events, returncode, stderr = stream(cmd, timeout + 60)

    proposal = None
    if PROPOSAL.exists():
        proposal = extract_json(PROPOSAL.read_text())
    if proposal is None:
        proposal = extract_json(last_assistant_text(events))
    if not isinstance(proposal, dict) or not isinstance(proposal.get("actions"), list):
        tail = (stderr or events)[-300:]
        finish({"error": f"no proposal JSON (exit {returncode}): {tail}"}, 1)
    finish({"summary": str(proposal.get("summary", ""))[:600], "actions": proposal["actions"][:4]}, 0)


if __name__ == "__main__":
    main()
