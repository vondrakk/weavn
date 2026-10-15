# Weavn gateway image — multi-stage.
# Build context is the repo root (so the whole Cargo workspace + the embedded
# weavn-store/migrations are available; `sqlx::migrate!()` bakes them into the
# binary at compile time). No DATABASE_URL is needed at build time — the store
# uses runtime sqlx::query(), not the compile-time-checked macros.

FROM rust:1-bookworm AS build
WORKDIR /src
# Copy the workspace. (.dockerignore keeps target/, site/, decks/ etc. out.)
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# real-jira is pulled in transitively by weavn-gateway's dependency on weavn.
# (Plain build — no BuildKit cache mounts, so this works identically under
# podman/buildah or docker.)
RUN cargo build --release -p weavn-gateway \
    && cp target/release/weavn-gateway /usr/local/bin/weavn-gateway

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --no-create-home weavn
COPY --from=build /usr/local/bin/weavn-gateway /usr/local/bin/weavn-gateway
USER weavn
ENV WEAVN_BIND=0.0.0.0:8080
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/weavn-gateway"]
