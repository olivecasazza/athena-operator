# Stateless research-proposer runner: Prime Agent (PrimeIntellect-ai/prime-agent,
# MIT) driven headlessly by a ResearchDrive. The drive controller creates one
# Job per proposal; this image reads Athena through its MCP endpoint, asks the
# drive's OpenAI-compatible model, and writes the proposal JSON to the pod
# termination message, which the controller parses and validates exactly like
# a single-call proposer reply. No Kubernetes credentials, no durable state.
FROM python:3.11-slim-bookworm

ARG PRIME_AGENT_VERSION=0.9.6
ARG PRIME_AGENT_SHA256=
ADD https://github.com/PrimeIntellect-ai/prime-agent/releases/download/v${PRIME_AGENT_VERSION}/prime-agent-${PRIME_AGENT_VERSION}-linux-x64.tar.gz /tmp/prime-agent.tar.gz
RUN set -eu; \
    if [ -n "$PRIME_AGENT_SHA256" ]; then echo "$PRIME_AGENT_SHA256  /tmp/prime-agent.tar.gz" | sha256sum -c -; fi; \
    mkdir -p /opt/prime-agent && tar xzf /tmp/prime-agent.tar.gz -C /opt/prime-agent && rm /tmp/prime-agent.tar.gz; \
    /opt/prime-agent/prime-agent --version

# Pre-build the RLM kernel so the Job needs no network beyond the model and
# MCP endpoints (the managed bootstrap would fetch Python + packages via uv).
# Package list mirrors DEFAULT_RLM_EXTRA_PACKAGES in prime-agent's
# kernel/bootstrap.ts; the kernel preflight rejects an env missing any.
RUN python -m venv /opt/kernel && \
    /opt/kernel/bin/pip install --no-cache-dir /opt/prime-agent/prime-agent-runtime dill \
      requests httpx pyyaml tomli python-dotenv pandas numpy scipy beautifulsoup4 lxml pydantic tyro

RUN useradd --uid 10001 --create-home runner
COPY entrypoint.py /opt/runner/entrypoint.py

ENV PRIME_AGENT_KERNEL_PYTHON=/opt/kernel/bin/python \
    PRIME_AGENT_CODING_AGENT_DIR=/work/.prime \
    HOME=/work \
    PATH=/opt/prime-agent:$PATH
USER 10001
WORKDIR /work
ENTRYPOINT ["python3", "/opt/runner/entrypoint.py"]
