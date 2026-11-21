# syntax=docker/dockerfile:1

# Hyperfocal ML: face and pet detection and recognition, search by content and eye checks, for Hyperfocal.
#   docker build -f ml/Dockerfile -t hyperfocal-ml .                         (CPU)
#   docker build -f ml/Dockerfile --build-arg VARIANT=cuda -t hyperfocal-ml:cuda .   (NVIDIA)

ARG VARIANT=cpu

# ---------------------------------------------------------------- models
# Their own stage: they change rarely, so code changes never download them again.
FROM python:3.12-slim-trixie AS models
WORKDIR /build
COPY ml/models.json ml/fetch_models.py ./
COPY ml/models ./models
RUN python fetch_models.py /models

# ---------------------------------------------------------------- runtime
FROM python:3.12-slim-trixie
ARG VARIANT
ENV PYTHONDONTWRITEBYTECODE=1 \
    PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1
COPY ml/requirements.txt ml/requirements-cuda.txt /tmp/
RUN if [ "$VARIANT" = "cuda" ]; then pip install -r /tmp/requirements-cuda.txt; else pip install -r /tmp/requirements.txt; fi \
 && rm /tmp/requirements*.txt
COPY --from=models /models /models
COPY ml/hyperfocal_ml /app/hyperfocal_ml
WORKDIR /app

# Loads every model once: fails the build if one cannot run.
RUN HFML_DEVICE=cpu python -c "from hyperfocal_ml.pipeline import Pipeline; p = Pipeline(); print(p.device, p.models)"

ENV HFML_MODELS=/models \
    HFML_PORT=8090 \
    HOME=/tmp

LABEL org.opencontainers.image.title="Hyperfocal ML" \
      org.opencontainers.image.description="Face and pet recognition and search by content for Hyperfocal (ONNX Runtime, CPU or NVIDIA GPU)" \
      org.opencontainers.image.source="https://github.com/au12/Hyperfocal" \
      org.opencontainers.image.licenses="MIT"

EXPOSE 8090
# Nothing is written: the service keeps no state.
USER 99:100
HEALTHCHECK --interval=30s --timeout=10s --start-period=90s --retries=3 \
  CMD ["python", "-m", "hyperfocal_ml", "healthcheck"]
CMD ["python", "-m", "hyperfocal_ml"]
