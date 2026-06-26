"""Athena live-metrics helper for experiment workloads.

Transparent store/retrieve of training metrics through the cluster Prometheus +
Grafana, wired by the Athena operator. The operator injects these env vars into
every experiment Job pod (from ``RuntimeProfile.metricsEndpoint``):

    ATHENA_METRICS_ENABLED      "true"/"false"
    ATHENA_METRICS_PORT         port to expose the Prometheus-text endpoint on
    ATHENA_METRICS_SCRAPE_PATH  HTTP path the metrics are served at (/metrics)
    ATHENA_PROMETHEUS_URL       base URL for queries ("" => retrieve disabled)
    ATHENA_EXPERIMENT           experiment name  (label on every series)
    ATHENA_CAMPAIGN             campaign name    (label on every series)

The ``athena-experiment`` PodMonitor scrapes the exposed port, so ``store`` is
just "set a gauge" — the scrape ships it to Prometheus during the run, and it
shows up in the Athena Experiment Debugging dashboard.

``retrieve``/``query`` reads back from Prometheus for **live display only**.
Decisions (best-so-far, keep/revert, gating) must read authoritative Athena CR
status / workspace artifacts, not this lossy scrape-sampled mirror; and the
final ``metrics.json`` summary stays authoritative regardless of what is stored
here.

No third-party dependency for the store path (stdlib ``http.server``); the
query path uses ``requests`` (already a project dependency). Every call is a
safe no-op when disabled or unset, so the same training script runs unchanged
locally and in-cluster.
"""

from __future__ import annotations

import os
import re
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

# Prometheus metric/label name rules: [a-zA-Z_][a-zA-Z0-9_]* (metric names may
# also contain ':' but we don't use it). Anything else becomes '_'.
_INVALID = re.compile(r"[^a-zA-Z0-9_]")


def _sanitize(name: str) -> str:
    safe = _INVALID.sub("_", str(name))
    if safe and safe[0].isdigit():
        safe = "_" + safe
    return safe or "_"


def _escape(value: str) -> str:
    return str(value).replace("\\", "\\\\").replace('"', '\\"').replace("\n", "\\n")


class AthenaMetrics:
    """Live metric sink (Prometheus-text endpoint) + display-only query client."""

    def __init__(self) -> None:
        self.enabled = os.getenv("ATHENA_METRICS_ENABLED", "false").lower() == "true"
        try:
            self.port = int(os.getenv("ATHENA_METRICS_PORT", "0") or 0)
        except ValueError:
            self.port = 0
        self.path = os.getenv("ATHENA_METRICS_SCRAPE_PATH", "/metrics") or "/metrics"
        self.prometheus_url = os.getenv("ATHENA_PROMETHEUS_URL", "").rstrip("/")

        self.base_labels: dict[str, str] = {}
        for key, env in (("experiment", "ATHENA_EXPERIMENT"), ("campaign", "ATHENA_CAMPAIGN")):
            val = os.getenv(env)
            if val:
                self.base_labels[key] = val

        # (metric, sorted-label-tuple) -> (metric, labels, value)
        self._gauges: dict[tuple, tuple[str, dict[str, str], float]] = {}
        self._lock = threading.Lock()
        self._server: ThreadingHTTPServer | None = None

        if self.enabled and self.port > 0:
            self._start_server()

    # -- store --------------------------------------------------------------

    def store(self, name: str, value: float, **labels: object) -> None:
        """Set the latest value of a gauge. Prefixed ``athena_`` so it lands in
        the Athena dashboards. No-op when metrics are disabled."""
        if not self.enabled:
            return
        metric = "athena_" + _sanitize(name)
        merged = dict(self.base_labels)
        for k, v in labels.items():
            merged[_sanitize(k)] = str(v)
        key = (metric, tuple(sorted(merged.items())))
        with self._lock:
            self._gauges[key] = (metric, merged, float(value))

    def _render(self) -> str:
        with self._lock:
            items = list(self._gauges.values())
        lines: list[str] = []
        typed: set[str] = set()
        for metric, labels, value in sorted(items, key=lambda i: i[0]):
            if metric not in typed:
                lines.append(f"# TYPE {metric} gauge")
                typed.add(metric)
            if labels:
                rendered = ",".join(f'{k}="{_escape(v)}"' for k, v in sorted(labels.items()))
                lines.append(f"{metric}{{{rendered}}} {value}")
            else:
                lines.append(f"{metric} {value}")
        return "\n".join(lines) + "\n"

    def _start_server(self) -> None:
        helper = self

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self) -> None:  # noqa: N802 (stdlib API)
                if self.path.split("?", 1)[0] != helper.path:
                    self.send_response(404)
                    self.end_headers()
                    return
                body = helper._render().encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/plain; version=0.0.4")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_args) -> None:  # silence access logs
                pass

        self._server = ThreadingHTTPServer(("0.0.0.0", self.port), Handler)
        threading.Thread(target=self._server.serve_forever, daemon=True).start()

    # -- retrieve (display only) -------------------------------------------

    def query(self, promql: str):
        """Instant Prometheus query for live display. Returns the parsed JSON
        ``data.result`` list, or ``None`` if retrieval is unavailable/failed.
        Do not drive control-flow decisions from this."""
        if not self.prometheus_url:
            return None
        try:
            import requests

            resp = requests.get(
                f"{self.prometheus_url}/api/v1/query",
                params={"query": promql},
                timeout=10,
            )
            resp.raise_for_status()
            payload = resp.json()
            if payload.get("status") == "success":
                return payload.get("data", {}).get("result", [])
        except Exception:
            return None
        return None


# Module-level singleton + convenience wrappers so workloads can just
# `from athena_metrics import store, query`.
_DEFAULT: AthenaMetrics | None = None


def metrics() -> AthenaMetrics:
    global _DEFAULT
    if _DEFAULT is None:
        _DEFAULT = AthenaMetrics()
    return _DEFAULT


def store(name: str, value: float, **labels: object) -> None:
    metrics().store(name, value, **labels)


def query(promql: str):
    return metrics().query(promql)


if __name__ == "__main__":
    # Self-check: store renders valid Prometheus text with merged labels, and a
    # disabled instance is a no-op.
    os.environ.update(
        ATHENA_METRICS_ENABLED="true",
        ATHENA_METRICS_PORT="0",  # don't bind a socket in the check
        ATHENA_EXPERIMENT="exp-1",
        ATHENA_CAMPAIGN="camp-1",
    )
    m = AthenaMetrics()
    m.store("train_loss", 1.5, step=42)
    m.store("train-loss", 1.25, step=43)  # name sanitized -> athena_train_loss
    text = m._render()
    assert 'athena_train_loss{campaign="camp-1",experiment="exp-1",step="43"} 1.25' in text, text
    assert "# TYPE athena_train_loss gauge" in text, text

    disabled = AthenaMetrics()  # env still enabled; force-disable
    disabled.enabled = False
    disabled.store("loss", 9.9)
    assert disabled._render() == "\n", repr(disabled._render())

    assert _sanitize("123abc")[0] == "_"
    assert query("up") is None or isinstance(query("up"), list)
    print("athena_metrics self-check ok")
