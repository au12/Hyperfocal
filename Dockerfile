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
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh
# Fails the build if the native decoders cannot load in the runtime image.
RUN hyperfocal native version | grep -q libraw

ENV HYPERFOCAL_PHOTOS=/photos \
    HYPERFOCAL_CONFIG=/config \
    HYPERFOCAL_WEB=/app/web \
    HYPERFOCAL_PORT=8080 \
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
