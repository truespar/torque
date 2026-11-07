# ── Build Studio ──────────────────────────────────────────────
FROM node:22-bookworm-slim AS studio

WORKDIR /src/studio
COPY studio/package.json studio/package-lock.json ./
RUN npm ci --ignore-scripts
COPY studio/ ./
RUN npm run build
# Output: /src/studio/dist/ (embedded into the binary at compile time)

# ── Build Server ─────────────────────────────────────────────
FROM rust:1.96-bookworm AS builder

WORKDIR /src

COPY Cargo.toml Cargo.lock build.rs ./
COPY .cargo/ .cargo/
COPY src/ src/
COPY benches/ benches/
COPY vendor/ vendor/
COPY ffi/ ffi/
COPY --from=studio /src/studio/dist studio/dist/

ENV RUSTFLAGS="--remap-path-prefix=/src=src"
RUN cargo build --release --features native,tls,mcp

# ── Runtime ──────────────────────────────────────────────────
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /src/target/release/torque-server /usr/local/bin/torque-server
COPY --from=builder /src/target/release/torque-admin /usr/local/bin/torque-admin

ENV TORQUE_API_ADDR=0.0.0.0:8108
ENV TORQUE_INGEST_ADDR=0.0.0.0:8109
ENV TORQUE_DATA_DIR=/data
# No fixed API key: a container generates one and prints it at startup (docker logs). Set
# TORQUE_API_KEY for a key of your own.

EXPOSE 8108 8109
VOLUME /data

ENTRYPOINT ["torque-server"]
CMD []
