FROM --platform=$BUILDPLATFORM ubuntu:24.04 AS builder

# Build arguments for cross-compilation
ARG BUILDPLATFORM
ARG TARGETPLATFORM
ARG TARGETARCH

# Install development tools and dependencies
RUN apt-get update && \
    apt-get install -y \
    build-essential \
    git \
    curl \
    pkg-config \
    protobuf-compiler \
    libprotobuf-dev \
    perl \
    make \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# Install additional tools for cross-compilation if needed
RUN if [ "$TARGETARCH" = "arm64" ] && [ "$BUILDPLATFORM" != "linux/arm64" ]; then \
        apt-get update && \
        apt-get install -y \
        gcc-aarch64-linux-gnu \
        g++-aarch64-linux-gnu && \
        rm -rf /var/lib/apt/lists/*; \
    elif [ "$TARGETARCH" = "amd64" ] && [ "$BUILDPLATFORM" != "linux/amd64" ]; then \
        apt-get update && \
        apt-get install -y \
        gcc-x86-64-linux-gnu \
        g++-x86-64-linux-gnu && \
        rm -rf /var/lib/apt/lists/*; \
    fi

# Install Rust and Cargo
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:$PATH"

# Add the appropriate Rust target based on TARGETARCH
RUN if [ "$TARGETARCH" = "arm64" ]; then \
        rustup target add aarch64-unknown-linux-gnu; \
    elif [ "$TARGETARCH" = "amd64" ]; then \
        rustup target add x86_64-unknown-linux-gnu; \
    fi

# Set working directory
WORKDIR /app

# Copy workspace root files
COPY Cargo.toml ./
COPY Cargo.lock ./

# Copy proto files (needed by proto crate build script)
COPY proto/ ./proto/

# Copy all crates
COPY crates/ ./crates/

# Build the cloud-agent in release mode for the target architecture
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true
RUN if [ "$TARGETARCH" = "arm64" ]; then \
        export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc && \
        export CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ && \
        export AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar && \
        export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc && \
        cargo build --release --target aarch64-unknown-linux-gnu -p orderbook-cloud-agent && \
        cp target/aarch64-unknown-linux-gnu/release/cloud-agent /tmp/cloud-agent; \
    elif [ "$TARGETARCH" = "amd64" ]; then \
        export CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc && \
        export CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++ && \
        export AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar && \
        export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc && \
        cargo build --release --target x86_64-unknown-linux-gnu -p orderbook-cloud-agent && \
        cp target/x86_64-unknown-linux-gnu/release/cloud-agent /tmp/cloud-agent; \
    else \
        cargo build --release -p orderbook-cloud-agent && \
        cp target/release/cloud-agent /tmp/cloud-agent; \
    fi

# Create output directory and copy binary
RUN mkdir -p /output && cp /tmp/cloud-agent /output/

# List the output
RUN ls -lh /output/ && echo "Binary built successfully for $TARGETARCH"

# The binary will be at /output/cloud-agent
# This can be extracted using docker cp when the container is created
