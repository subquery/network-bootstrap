# Builder
FROM rust:bullseye AS builder
RUN update-ca-certificates
ENV CARGO_NET_GIT_FETCH_WITH_CLI=true

# Update deps
RUN apt update
RUN apt install -y curl wget build-essential python3 pkg-config ca-certificates libssl-dev

# Configure postgres
USER root

WORKDIR /subql

COPY . .
RUN cargo build --release

# Final image
FROM debian:bullseye-slim

RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && \
    apt-get --assume-yes install curl && \
    update-ca-certificates

WORKDIR /subql

# Copy our build
COPY --from=builder /subql/target/release/network_bootstrap .

# Use an unprivileged user.
RUN groupadd --gid 10001 subql && \
    useradd  --home-dir /subql \
             --create-home \
             --shell /bin/bash \
             --gid subql \
             --groups subql \
             --uid 10000 subql
RUN mkdir -p /subql/.local/share && \
	mkdir /subql/data && \
	chown -R subql:subql /subql && \
	ln -s /subql/data /subql/.local/share
USER subql:subql

ENTRYPOINT ["./network_bootstrap"]
