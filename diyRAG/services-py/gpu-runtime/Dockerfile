# diyRAG gpu-runtime — inference sidecar image (MASTER_BUILD_SPEC.md §12.8, §16, §19).
#
# CUDA BASE NOTE (§16): the runtime needs a CUDA runtime matching torch 2.5.1
# (CUDA 12.1). The default base below is the NVIDIA CUDA 12.1 runtime image. On
# CPU-only / Windows nodes this container is NOT used — the Rust-native backend
# runs instead (§24.1), so there is no CPU variant of this image by design.
#
# Pin the digest in production (§12.8: pinned base images). The tag is given
# here for readability; replace with `@sha256:...` in CI.
ARG CUDA_IMAGE=nvidia/cuda:12.1.1-cudnn8-runtime-ubuntu22.04
FROM ${CUDA_IMAGE}

# ---------------------------------------------------------------------------
# OS + Python. No build toolchain in the final image beyond what wheels need.
# ---------------------------------------------------------------------------
ENV DEBIAN_FRONTEND=noninteractive \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PIP_NO_CACHE_DIR=1 \
    HF_HOME=/models \
    DIYRAG_GPU_HOST=0.0.0.0 \
    DIYRAG_GPU_PORT=8081

RUN apt-get update \
    && apt-get install -y --no-install-recommends python3.11 python3-pip python3.11-venv \
    && ln -sf /usr/bin/python3.11 /usr/local/bin/python \
    && rm -rf /var/lib/apt/lists/*

# ---------------------------------------------------------------------------
# Non-root user (§12.8). The model cache lives under a dir this user owns; it is
# a mounted volume in prod so weights are not baked into the image.
# ---------------------------------------------------------------------------
RUN groupadd --gid 10001 diyrag \
    && useradd --uid 10001 --gid diyrag --create-home --home-dir /home/diyrag diyrag \
    && mkdir -p /models /app \
    && chown -R diyrag:diyrag /models /app

WORKDIR /app

# Dependency install. torch comes from the CUDA wheel index to match the base
# image (§16). Build arg lets the `py-llm` / `ocr` profiles layer extras.
COPY --chown=diyrag:diyrag requirements.txt requirements-vllm.txt requirements-ocr.txt ./
ARG INSTALL_VLLM=false
ARG INSTALL_OCR=false
RUN python -m pip install --upgrade pip \
    && python -m pip install torch==2.5.1 --index-url https://download.pytorch.org/whl/cu121 \
    && python -m pip install -r requirements.txt \
    && if [ "$INSTALL_VLLM" = "true" ]; then python -m pip install vllm==0.6.6; fi \
    && if [ "$INSTALL_OCR" = "true" ]; then python -m pip install surya-ocr==0.8.3 marker-pdf==1.2.3; fi

COPY --chown=diyrag:diyrag app ./app

USER diyrag

EXPOSE 8081

# Liveness/readiness are also wired as container healthchecks (§0). Readiness
# (/readyz) only returns 200 once models are resident.
HEALTHCHECK --interval=30s --timeout=5s --start-period=120s --retries=3 \
    CMD python -c "import urllib.request,sys; sys.exit(0 if urllib.request.urlopen('http://127.0.0.1:8081/healthz').status==200 else 1)"

# Single process owns the GPU (§16 single point of GPU entry); no multi-worker
# fan-out — that would contend for the device.
CMD ["python", "-m", "uvicorn", "app.main:app", "--host", "0.0.0.0", "--port", "8081", "--log-config", "/dev/null"]
