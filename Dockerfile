# =============================================================================
# Stage 1: Builder — компиляция в полном rust-образе
# =============================================================================
FROM rust:1.80-slim AS builder

WORKDIR /app

# Системные зависимости для сборки
RUN apt-get update && apt-get install -y pkg-config libssl-dev gcc && rm -rf /var/lib/apt/lists/*

# Кэшируем зависимости: копируем Cargo.* и создаём фиктивный main.rs
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && \
    cargo build --release 2>&1 && \
    rm -rf src

# Настоящий исходный код
COPY . .
RUN cargo build --release

# =============================================================================
# Stage 2: Runtime — минимальный образ с docker CLI
# =============================================================================
FROM debian:bookworm-slim

# CA-сертификаты + Docker CLI для взаимодействия с другими контейнерами
RUN apt-get update && apt-get install -y ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    curl -fsSL https://get.docker.com | sh

WORKDIR /app
COPY --from=builder /app/target/release/ai-agent /app/ai-agent

EXPOSE 8080

ENTRYPOINT ["/app/ai-agent"]
