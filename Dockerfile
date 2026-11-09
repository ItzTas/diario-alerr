FROM python:3.14-slim-trixie

# poppler-utils is left unpinned on purpose: Debian drops superseded versions on every security
# update, so a pinned version would break rebuilds.
# hadolint ignore=DL3008
RUN apt-get update \
    && apt-get install -y --no-install-recommends poppler-utils \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o 2000 -g 2000 /data

WORKDIR /app
COPY monitor.py .

ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1

# the host's `stacks` user (uid 2000, no shell, no sudo); the data volume inherits /data's owner
USER 2000:2000
EXPOSE 8080
HEALTHCHECK --interval=60s --timeout=10s --start-period=30s --retries=3 \
    CMD ["python", "-I", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8080/healthz', timeout=5)"]
CMD ["python", "-I", "monitor.py"]
