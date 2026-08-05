# ---- chef base ----
# Předpřipravený image s cargo-chef, ať se nemusí na každém buildu
# instalovat rustup a kompilovat cargo-chef ze zdrojáků.
FROM lukemathwalker/cargo-chef:latest-rust-1.90-slim-bookworm AS chef
WORKDIR /app

# ---- planner ----
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---- builder ----
FROM chef AS builder
# Dotazy se ověřují proti cachi v .sqlx, ne proti živé DB. Explicitně,
# aby build nespadl, kdyby prostředí injectlo DATABASE_URL jako build variable.
ENV SQLX_OFFLINE=true
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN cargo build --release

# ---- runtime ----
FROM debian:bookworm-slim

# curl kvůli healthchecku; ca-certificates zůstávají pro jistotu,
# i když rustls jede na vestavěných webpki roots.
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Aplikace nepotřebuje roota.
RUN useradd --system --create-home --uid 10001 app

WORKDIR /app
COPY --from=builder /app/target/release/ivan-scraper /usr/local/bin/app
COPY --from=builder /app/frontend ./frontend

USER app

EXPOSE 3000

CMD ["app"]
