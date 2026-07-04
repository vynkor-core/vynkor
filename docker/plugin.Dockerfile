# Demo plugin image: runs the sdk/python echo_plugin example against a
# kernel reachable over a shared UDS socket volume (see ../docker-compose.yaml).
# Lives here, not inside sdk/python, because that dir is a separate git
# submodule (veyron-sdk-python) — this repo shouldn't add files to it.

FROM python:3.12-slim
WORKDIR /app
COPY pyproject.toml README.md LICENSE ./
COPY veyron ./veyron
COPY examples ./examples
RUN pip install --no-cache-dir .

# uid must match the kernel image's uid (10001) — the kernel's UDS socket is
# bound 0600, so only that exact uid can open it for connect().
RUN useradd --system --create-home --uid 10001 veyron
USER veyron

CMD ["python", "-m", "examples.echo_plugin"]
