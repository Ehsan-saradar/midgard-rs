# Build.
#
# Manifests are copied and built against stub sources first so that the
# dependency compile - by far the slowest part - is cached and only redone when
# Cargo.toml or Cargo.lock actually change.
FROM rust:1.88-slim-bookworm AS build

RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /src

COPY Cargo.toml Cargo.lock ./
COPY crates/midgard-core/Cargo.toml   crates/midgard-core/Cargo.toml
COPY crates/midgard-config/Cargo.toml crates/midgard-config/Cargo.toml
COPY crates/midgard-db/Cargo.toml     crates/midgard-db/Cargo.toml
COPY crates/midgard-chain/Cargo.toml  crates/midgard-chain/Cargo.toml
COPY crates/midgard-record/Cargo.toml crates/midgard-record/Cargo.toml
COPY crates/midgard-api/Cargo.toml    crates/midgard-api/Cargo.toml
COPY crates/midgard/Cargo.toml        crates/midgard/Cargo.toml

RUN for c in midgard-core midgard-config midgard-db midgard-chain midgard-record midgard-api; do \
        mkdir -p crates/$c/src && echo "" > crates/$c/src/lib.rs; \
    done \
 && mkdir -p crates/midgard/src \
 && echo "fn main() {}" > crates/midgard/src/main.rs \
 && mkdir -p crates/midgard-db/sql && touch crates/midgard-db/sql/ddl.sql \
 && cargo build --release --locked \
 && rm -rf crates/*/src

COPY crates crates
# Cargo caches by mtime, so the stubs have to be visibly superseded.
RUN find crates -name '*.rs' -exec touch {} + \
 && cargo build --release --locked --bin midgard

# Run.
FROM debian:bookworm-slim

RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --create-home --uid 10001 midgard

WORKDIR /app
COPY --from=build /src/target/release/midgard /usr/local/bin/midgard
COPY config config

USER midgard
EXPOSE 8080

ENTRYPOINT ["midgard"]
CMD ["config/base.json", "config/pg.json", "config/net-main.json"]
