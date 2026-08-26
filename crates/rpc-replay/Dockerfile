# syntax=docker/dockerfile:1.7
# Multi-stage build for rpc-replay
FROM --platform=linux/amd64 ubuntu:22.04 AS builder

# Set environment variables
ENV DEBIAN_FRONTEND=noninteractive
ENV RUSTUP_HOME=/usr/local/rustup
ENV CARGO_HOME=/usr/local/cargo
ENV PATH=/usr/local/cargo/bin:$PATH

# Install system dependencies
RUN apt-get update && apt-get install -y \
    build-essential \
    curl \
    pkg-config \
    libssl-dev \
    libgmp-dev \
    python3 \
    python3-pip \
    python3-venv \
    python3-dev \
    git \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Install Rust for linux/amd64.
# Keep the image builder aligned with the workspace toolchain used for local validation.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain 1.89 \
    && . /usr/local/cargo/env \
    && rustup target add x86_64-unknown-linux-gnu

# Set the working directory
WORKDIR /app

# Copy the entire workspace
COPY . .

# Create Python virtual environment (force recreation)
RUN rm -rf sequencer_venv || true
RUN python3 -m venv sequencer_venv

# Verify venv was created correctly
RUN ls -la sequencer_venv/bin/

# Install Python dependencies
RUN sequencer_venv/bin/pip install --upgrade pip

# Fetch cargo dependencies first
RUN . /usr/local/cargo/env && cargo fetch || true

# Install requirements if found
RUN if find /usr/local/cargo/git/checkouts -name "requirements.txt" -path "*/scripts/*" 2>/dev/null | head -1; then \
        REQS_FILE=$(find /usr/local/cargo/git/checkouts -name "requirements.txt" -path "*/scripts/*" | head -1); \
        echo "Found requirements.txt at: $REQS_FILE"; \
        sequencer_venv/bin/pip install -r "$REQS_FILE"; \
    else \
        echo "No requirements.txt found, skipping pip install"; \
    fi

# Apollo 0.14.2 currently resolves tblgen 0.5.2, which requires LLVM 19.
RUN apt-get update \
    && apt-get install -y lsb-release wget software-properties-common gnupg \
    && curl -fsSL https://apt.llvm.org/llvm.sh -o /tmp/llvm.sh \
    && chmod +x /tmp/llvm.sh \
    && /tmp/llvm.sh 19 all \
    && apt-get install -y libmlir-19-dev mlir-19-tools libzstd-dev \
    && test -x /usr/bin/llvm-config-19 \
    && /usr/bin/llvm-config-19 --version \
    && rm -f /tmp/llvm.sh \
    && rm -rf /var/lib/apt/lists/*

ENV LLVM_CONFIG_PATH=/usr/bin/llvm-config-19
ENV TABLEGEN_190_PREFIX=/usr/lib/llvm-19
ENV MLIR_SYS_190_PREFIX=/usr/lib/llvm-19
ENV LLVM_SYS_191_PREFIX=/usr/lib/llvm-19

# Apollo's build script manages this executable in a shared target directory and
# can race with its own nested Cargo install. Seed the exact required compiler so
# the build script takes its read-only, already-current path.
RUN --mount=type=cache,target=/app/target \
    . sequencer_venv/bin/activate \
    && . /usr/local/cargo/env \
    && mkdir -p /app/target/release/shared_executables \
    && if ! /app/target/release/shared_executables/starknet-native-compile --version 2>/dev/null \
        | grep -F '0.9.0-rc.6'; then \
        rm -f /app/target/release/shared_executables/starknet-native-compile; \
        rm -rf /tmp/starknet-native-compiler; \
        CARGO_TARGET_DIR=/app/target cargo install \
          --root /tmp/starknet-native-compiler \
          --locked \
          starknet-native-compile \
          --version 0.9.0-rc.6; \
        install -m 0755 \
          /tmp/starknet-native-compiler/bin/starknet-native-compile \
          /app/target/release/shared_executables/starknet-native-compile; \
    fi \
    && /app/target/release/shared_executables/starknet-native-compile --version \
      | grep -F '0.9.0-rc.6'

# Build the release binary serially so Apollo's remaining generated build tools
# do not contend for their shared target directory.
RUN --mount=type=cache,target=/app/target \
    . sequencer_venv/bin/activate \
    && . /usr/local/cargo/env \
    && cargo build --release --bin rpc-replay --jobs 1 \
    && cp /app/target/release/rpc-replay /app/rpc-replay

# Runtime stage
FROM --platform=linux/amd64 ubuntu:22.04

ARG SOURCE_REVISION=unknown
ARG SOURCE_DIFF_SHA256=unknown

LABEL org.opencontainers.image.revision="$SOURCE_REVISION" \
      io.karnot.snos.source-diff-sha256="$SOURCE_DIFF_SHA256" \
      io.karnot.snos.starknet-version="0.14.2"

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    libgmp10 \
    python3 \
    python3-venv \
    && rm -rf /var/lib/apt/lists/* \
    && apt-get clean

# Create a non-root user
RUN useradd -m -u 1000 appuser

# Create directories for logs and output
RUN mkdir -p /app/output /app/logs && chown -R appuser:appuser /app

# Copy the release binary out of the ephemeral build cache.
COPY --from=builder /app/rpc-replay /usr/local/bin/rpc-replay

# Copy Python virtual environment
COPY --from=builder /app/sequencer_venv /app/sequencer_venv

# COPY the bash script
COPY --from=builder /app/crates/rpc-replay/rpc-replay-binary.sh /app/rpc-replay-binary.sh

# Set proper permissions
RUN chmod +x /usr/local/bin/rpc-replay
RUN chmod +x /app/rpc-replay-binary.sh

# Switch to non-root user
USER appuser

# Set working directory
WORKDIR /app

# Set default environment variables (can be overridden at runtime)
ENV RUST_LOG=info
ENV RUST_BACKTRACE=1
ENV RUST_LOG_STYLE=always
ENV AWS_S3_BUCKET=""
ENV AWS_ACCESS_KEY_ID=""
ENV AWS_SECRET_ACCESS_KEY=""
ENV PATH=/app/sequencer_venv/bin:$PATH

# Expose volumes for output files and logs
VOLUME ["/app/output", "/app/logs"]

# Health check to ensure the application is running
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD pgrep rpc-replay || exit 1

# Default command - passes all arguments to the binary
ENTRYPOINT ["/app/rpc-replay-binary.sh"]
CMD ["--help"]
