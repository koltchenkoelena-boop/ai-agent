# =============================================================================
# Single-stage: копируем бинарник, собранный локально (не требует rust-образа)
# =============================================================================
# Сборка: cargo build --release
# Сборка образа: docker build -t native-ai-agent .
# =============================================================================
FROM debian:bookworm-slim

# CA-сертификаты + Docker CLI для взаимодействия с другими контейнерами
RUN apt-get update && apt-get install -y ca-certificates curl && \
    rm -rf /var/lib/apt/lists/* && \
    curl -fsSL https://get.docker.com | sh

WORKDIR /app
COPY target/release/ai-agent /app/ai-agent

EXPOSE 8080

ENTRYPOINT ["/app/ai-agent"]
