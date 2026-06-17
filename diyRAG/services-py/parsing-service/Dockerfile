# diyRAG parsing-service — hard-parse sidecar image (MASTER_BUILD_SPEC.md §12.8, §16, §19).
#
# CUDA base for GPU OCR (Surya/Marker — §16). On nodes that only need Docling
# layout parsing a CPU base works; the GPU base is the default since OCR is the
# common reason to cross the Python boundary (§6.3). Pin the digest in CI
# (§12.8). NOTE: no LibreOffice/Calibre here — the Rust worker spawns those
# (§3.2, §6.3), so this image stays free of those large suites.
ARG CUDA_IMAGE=nvidia/cuda:12.1.1-cudnn8-runtime-ubuntu22.04
FROM ${CUDA_IMAGE}

ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PIP_NO_CACHE_DIR=1 \
    HF_HOME=/models \
    DIYRAG_PARSING_HOST=0.0.0.0 \
    DIYRAG_PARSING_PORT=8082

RUN apt-get update \
    && apt-get install -y --no-install-recommends python3.11 python3-pip python3.11-venv \
    && ln -sf /usr/bin/python3.11 /usr/local/bin/python \
    && rm -rf /var/lib/apt/lists/*

# Non-root user (§12.8). Weights mount in via a volume; never baked in.
RUN groupadd --gid 10002 diyrag \
    && useradd --uid 10002 --gid diyrag --create-home --home-dir /home/diyrag diyrag \
    && mkdir -p /models /app \
    && chown -R diyrag:diyrag /models /app

WORKDIR /app

COPY --chown=diyrag:diyrag requirements.txt ./
RUN python -m pip install --upgrade pip \
    && python -m pip install torch==2.5.1 --index-url https://download.pytorch.org/whl/cu121 \
    && python -m pip install -r requirements.txt

COPY --chown=diyrag:diyrag app ./app
COPY --chown=diyrag:diyrag proto ./proto

USER diyrag

EXPOSE 8082

HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD python -c "import urllib.request,sys; sys.exit(0 if urllib.request.urlopen('http://127.0.0.1:8082/healthz').status==200 else 1)"

CMD ["python", "-m", "uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8082", "--log-config", "/dev/null"]
