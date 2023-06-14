FROM lukemathwalker/cargo-chef:latest-rust-1.70 AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder 
COPY --from=planner /app/recipe.json recipe.json
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --recipe-path recipe.json
# Build application
COPY . .
RUN cargo build --release

# We do not need the Rust toolchain to run the binary!
FROM debian:buster-slim AS runtime
WORKDIR /app
COPY --from=builder /app/target/release/matchmaking-server-rust /usr/local/bin
ENV IP_SOURCE=RightmostXForwardedFor
ENTRYPOINT ["/usr/local/bin/matchmaking-server-rust"]