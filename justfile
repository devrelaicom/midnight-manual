# midnight-manual — common development commands
# Install just: https://github.com/casey/just

default:
    @just --list

# === Quality gates (matches CI) ===

check: fmt-check clippy test
    @echo "✓ All checks passed"

# MSRV is pinned to 1.91.0 in rust-toolchain.toml, Cargo.toml, and clippy.toml.
# Keep this version in sync with those three files when bumping.
check-msrv:
    rustup run 1.91.0 cargo check --workspace --all-targets --all-features

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

test:
    cargo test --workspace --all-features --no-fail-fast

test-integration:
    cargo test --workspace --features integration --no-fail-fast

test-canary:
    cargo test --workspace --features canary --test canary -- --nocapture

# === Benchmarks ===

bench:
    cargo bench --workspace

# === Database ===

DATABASE_URL := env_var_or_default("DATABASE_URL", "postgresql://postgres:dev@localhost:5432/postgres")

migrate-up:
    sqlx migrate run --source crates/mnm-store/migrations --database-url "{{DATABASE_URL}}"

migrate-status:
    sqlx migrate info --source crates/mnm-store/migrations --database-url "{{DATABASE_URL}}"

# Refresh sqlx offline query cache (commit the result)
sqlx-prepare:
    cargo sqlx prepare --workspace -- --tests

# Boot a local Postgres + pgvector via Docker
db-up:
    docker run -d --name mn-postgres -p 5432:5432 \
        -e POSTGRES_PASSWORD=dev \
        pgvector/pgvector:pg16

db-down:
    docker stop mn-postgres && docker rm mn-postgres

# === Builds ===

build-cli:
    cargo build --release -p midnight-manual

build-server:
    cargo build --release -p midnight-manual-server

# === Server (local) ===

run-server:
    cargo run --release -p midnight-manual-server

# === Release rehearsal ===

dist-plan:
    cargo dist plan

dist-build:
    cargo dist build

# === Security & supply chain ===

audit:
    cargo audit

deny:
    cargo deny check

# === Documentation ===

doc:
    cargo doc --workspace --no-deps --all-features --open
