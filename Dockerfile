# syntax=docker/dockerfile:1

# Hyperfocal: fast, self-hosted RAW photo culling and library manager.
#   docker build -t hyperfocal .

# ---------------------------------------------------------------- web UI
FROM node:24-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# ---------------------------------------------------------------- server
FROM rust:1-trixie AS server
# cmake + nasm: libjpeg-turbo is built from source with its SIMD code paths.
# libheif / LibRaw headers and libclang: HEIC support and the LibRaw bindings.
RUN apt-get update \
 && apt-get install -y --no-install-recommends cmake nasm pkg-config libclang-dev libheif-dev libraw-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
# Dependencies first, in their own layer, so source edits rebuild in seconds.
COPY server/Cargo.toml server/Cargo.lock server/build.rs ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
 && cargo build --release --locked \
 && rm -rf src
COPY server/src ./src
# touch: the placeholder above is newer than the checked-out sources.
RUN touch src/main.rs && cargo build --release --locked \
 && ./target/release/hyperfocal --version | grep -q "^hyperfocal "

# ---------------------------------------------------------------- ONNX Runtime
# The AI engine's runtime for the CPU: the C library from the official Python wheel,
# checked against its sha256 (the same file as in server/src/ml/manifest.json, which a
# server test keeps in step). The models and, for an NVIDIA GPU, ONNX Runtime's CUDA build
# are not in the image: Hyperfocal downloads them into /config/ml when they are needed.
FROM debian:trixie-slim AS ort
ARG TARGETARCH
ARG ORT_WHEEL_AMD64=https://files.pythonhosted.org/packages/83/2c/d8eb945d2a372149df9705a8d5c8d7c6c46c987c5446dbcea9e1ea7f6556/onnxruntime-1.29.0-cp313-cp313-manylinux_2_28_x86_64.whl
ARG ORT_SHA256_AMD64=e2128f31f449e922c62dbe5d8b6b7b079f0bcaf2d56a102fa203cb6e5bb5ab19
ARG ORT_WHEEL_ARM64=https://files.pythonhosted.org/packages/c9/17/b9ad04051a8c4f504852ce0e8e10f9a6b2f1a331eedcdcc503df776dd0ea/onnxruntime-1.29.0-cp313-cp313-manylinux_2_28_aarch64.whl
ARG ORT_SHA256_ARM64=d67673c5367727860922c5262d724472f1b5539fb7ccf4c81a638f9b71719803
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl unzip \
 && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    if [ "$TARGETARCH" = "arm64" ]; then url="$ORT_WHEEL_ARM64"; sum="$ORT_SHA256_ARM64"; else url="$ORT_WHEEL_AMD64"; sum="$ORT_SHA256_AMD64"; fi; \
    curl -fsSL --retry 5 -o /tmp/ort.whl "$url"; \
    echo "$sum  /tmp/ort.whl" | sha256sum -c -; \
    mkdir -p /ort; \
    unzip -j -o /tmp/ort.whl onnxruntime/capi/libonnxruntime.so.1.29.0 -d /ort; \
    rm /tmp/ort.whl

# ---------------------------------------------------------------- runtime
FROM debian:trixie-slim
# exiftool writes XMP sidecars (and nothing else); tini reaps child processes;
# libheif (with its HEVC plugin) and LibRaw run only inside the decoder helper process;
# tzdata: the TZ variable sets the local time of the nightly integrity check.
RUN apt-get update \
 && apt-get install -y --no-install-recommends libimage-exiftool-perl tini util-linux tzdata \
      libheif1 libheif-plugin-libde265 libraw23t64 \
 && rm -rf /var/lib/apt/lists/*

COPY --from=server /src/target/release/hyperfocal /usr/local/bin/hyperfocal
COPY --from=web /web/dist /app/web
COPY --from=ort /ort/libonnxruntime.so.1.29.0 /usr/lib/hyperfocal/libonnxruntime.so.1.29.0
# The two small eye models (no ONNX release upstream: converted once, see ml/models).
COPY ml/models/face-landmarks.onnx ml/models/face-blendshapes.onnx ml/models/LICENSE-mediapipe /app/ml/models/
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
# Fails the build if the native decoders cannot load in the runtime image.
RUN hyperfocal native version | grep -q libraw
# Fails the build if ONNX Runtime cannot load, or the bundled eye models give wrong
# answers on two test portraits (one with open eyes, one with closed).
RUN --mount=type=bind,source=server/tests/fixtures/ml,target=/tmp/mltest \
    hyperfocal ml-engine selftest /tmp/mltest

# NVIDIA_DRIVER_CAPABILITIES: with --runtime=nvidia (and NVIDIA_VISIBLE_DEVICES), the
# NVIDIA container runtime brings the driver's CUDA library, for HYPERFOCAL_ML_DEVICE.
ENV HYPERFOCAL_PHOTOS=/photos \
    HYPERFOCAL_CONFIG=/config \
    HYPERFOCAL_WEB=/app/web \
    HYPERFOCAL_PORT=8080 \
    NVIDIA_DRIVER_CAPABILITIES=compute,utility \
    PUID=99 \
    PGID=100 \
    UMASK=000 \
    HOME=/tmp

LABEL org.opencontainers.image.title="Hyperfocal" \
      org.opencontainers.image.description="Fast, self-hosted RAW photo culling and library manager" \
      org.opencontainers.image.source="https://github.com/au12/Hyperfocal" \
      org.opencontainers.image.licenses="MIT"

EXPOSE 8080
VOLUME ["/config"]
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s --retries=3 \
  CMD ["/usr/local/bin/hyperfocal", "healthcheck"]
ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/entrypoint.sh"]
CMD ["/usr/local/bin/hyperfocal"]
