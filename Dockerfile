# ---- Builder ----
FROM rust:1-bookworm AS builder

RUN apt-get update && apt-get install -y \
    build-essential \
    cmake \
    git \
    pkg-config \
    libclang-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Cache dependency build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
RUN mkdir src && echo 'fn main() { println!("stub"); }' > src/main.rs
RUN cargo build --release && rm -rf src target/release/deps/zcash_devtool*

# Build real binary
COPY src/ src/
RUN cargo build --release

# ---- Runtime ----
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y \
    ca-certificates \
    tini \
    curl \
    && rm -rf /var/lib/apt/lists/*

RUN groupadd -g 1000 zcash && useradd -u 1000 -g zcash -m zcash
RUN mkdir -p /data && chown zcash:zcash /data

COPY --from=builder /app/target/release/zcash-devtool /usr/local/bin/zcash-devtool

USER zcash
VOLUME /data
EXPOSE 8080

ENTRYPOINT ["tini", "--"]
CMD ["zcash-devtool", "serve", "--bind", "0.0.0.0:8080", "--data-dir", "/data"]
