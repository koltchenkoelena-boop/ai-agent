"""
Модуль авторизации для AI Agent.

Зеркалирует логику Rust-проекта ai-agent (src/provider.rs):
- Пул провайдеров собирается из переменных окружения
- Round-robin ротация эндпоинтов/ключей через CredentialRotator
- Bearer-токен для HTTP-заголовков Authorization

Используются только стандартные средства Python — никаких внешних зависимостей.
"""

from __future__ import annotations

import itertools
import logging
import os
import threading
from dataclasses import dataclass, field
from typing import Dict, List, Optional

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# AuthError
# ---------------------------------------------------------------------------


class AuthError(Exception):
    """Кастомное исключение для ошибок авторизации и конфигурации провайдеров."""

    pass


# ---------------------------------------------------------------------------
# AuthProvider
# ---------------------------------------------------------------------------


@dataclass
class AuthProvider:
    """Конфигурация одного провайдера (аналог ProviderConfig из Rust)."""

    name: str
    """Человеческое имя (для логов)."""

    base_url: str
    """Базовый URL API (например, http://localhost:11434)."""

    model_name: str
    """Имя модели, подставляемое в поле 'model' JSON-запроса."""

    api_key: Optional[str] = None
    """Bearer API-ключ (если требуется)."""

    supports_embeddings: bool = True
    """Поддерживает ли провайдер эмбеддинги (/api/embeddings)."""


# ---------------------------------------------------------------------------
# CredentialRotator
# ---------------------------------------------------------------------------


class CredentialRotator:
    """
    Потокобезопасный round-robin ротатор эндпоинтов/ключей.

    Аналог одноимённого типа из src/provider.rs (Rust), использующий
    threading.Lock для атомарного инкремента счетчика.
    """

    def __init__(self, endpoints: List[str]) -> None:
        """
        Инициализировать ротатор списком эндпоинтов.

        Args:
            endpoints: Непустой список URL или ключей.

        Raises:
            AuthError: Если передан пустой список.
        """
        if not endpoints:
            raise AuthError("CredentialRotator requires at least one endpoint")
        self._endpoints = list(endpoints)
        self._lock = threading.Lock()
        self._counter = 0

    def get_next(self) -> str:
        """
        Вернуть следующий эндпоинт в round-robin порядке.

        Returns:
            Следующий URL или ключ из пула.

        Raises:
            AuthError: Если пул пуст (не должно происходить после успешного
                       создания через __init__).
        """
        if not self._endpoints:
            raise AuthError("CredentialRotator pool is empty")
        with self._lock:
            idx = self._counter % len(self._endpoints)
            self._counter += 1
            return self._endpoints[idx]

    @property
    def endpoints(self) -> List[str]:
        """Вернуть копию списка эндпоинтов."""
        return list(self._endpoints)

    def __len__(self) -> int:
        return len(self._endpoints)

    def __repr__(self) -> str:
        return (
            f"CredentialRotator(endpoints={self._endpoints!r}, "
            f"counter={self._counter})"
        )


# ---------------------------------------------------------------------------
# AuthManager
# ---------------------------------------------------------------------------

# Значения по умолчанию — полные аналоги Rust-констант
_DEFAULT_MODEL = "qwen2.5-coder:7b"
_DEFAULT_OPENROUTER_MODEL = "qwen/qwen-2.5-coder-32b-instruct:free"
_DEFAULT_OLLAMA_URL = "http://localhost:11434"
_DEFAULT_OPENROUTER_URL = "https://openrouter.ai/api/v1"


class AuthManager:
    """
    Главный класс для управления авторизацией и пулом провайдеров.

    Загружает конфигурацию из переменных окружения, строит пул провайдеров
    и предоставляет методы для получения HTTP-заголовков с Bearer-токеном.

    Порядок приоритета провайдеров (совпадает с Rust build_provider_pool):
    1. AGENT_PROVIDER_POOL — явный список эндпоинтов
    2. OPENROUTER_API_KEY — OpenRouter (если задан ключ)
    3. Локальная Ollama — всегда как финальный fallback
    """

    def __init__(self) -> None:
        self._providers: List[AuthProvider] = []
        self._rotator: Optional[CredentialRotator] = None
        self._reload()

    # -----------------------------------------------------------------------
    # Публичные методы
    # -----------------------------------------------------------------------

    def get_headers(self, provider: AuthProvider) -> Dict[str, str]:
        """
        Сформировать HTTP-заголовки для запроса к указанному провайдеру.

        Если у провайдера задан api_key, в заголовки добавляется
        ``Authorization: Bearer <key>``.

        Args:
            provider: Провайдер, для которого формируются заголовки.

        Returns:
            Словарь HTTP-заголовков.
        """
        headers: Dict[str, str] = {
            "Content-Type": "application/json",
        }
        if provider.api_key:
            headers["Authorization"] = f"Bearer {provider.api_key}"
        return headers

    def list_providers(self) -> List[AuthProvider]:
        """
        Вернуть копию текущего списка настроенных провайдеров.

        Returns:
            Список AuthProvider.
        """
        return list(self._providers)

    def get_rotator(self) -> Optional[CredentialRotator]:
        """
        Вернуть ротатор эндпоинтов (если пул содержит несколько провайдеров
        с одинаковой auth-логикой и требуется round-robin).

        Returns:
            CredentialRotator или None, если ротация не настроена.
        """
        return self._rotator

    def reload(self) -> None:
        """
        Принудительно перечитать переменные окружения и перестроить пул.

        Вызывается автоматически при создании AuthManager; может быть
        полезен, если переменные окружения изменились во время работы.
        """
        self._reload()

    # -----------------------------------------------------------------------
    # Внутренние методы
    # -----------------------------------------------------------------------

    def _reload(self) -> None:
        """Прочитать переменные окружения и собрать пул провайдеров."""
        providers: List[AuthProvider] = []

        default_model = os.environ.get("AI_AGENT_MODEL", _DEFAULT_MODEL)
        ollama_api_key: Optional[str] = os.environ.get("OLLAMA_API_KEY") or None

        # 1. AGENT_PROVIDER_POOL — явный пул эндпоинтов
        pool_raw = os.environ.get("AGENT_PROVIDER_POOL", "").strip()
        if pool_raw:
            # Парсинг: разделение по запятой, очистка от скобок/кавычек
            parts = [
                p.strip().strip("[]\"'")
                for p in pool_raw.split(",")
            ]
            parts = [p for p in parts if p]
            for i, ep in enumerate(parts):
                providers.append(
                    AuthProvider(
                        name=f"pool-{i}",
                        base_url=ep,
                        model_name=default_model,
                        api_key=ollama_api_key,
                        supports_embeddings=True,
                    )
                )

        # 2. OpenRouter (если задан OPENROUTER_API_KEY)
        or_key = os.environ.get("OPENROUTER_API_KEY", "").strip()
        if or_key:
            or_model = os.environ.get(
                "OPENROUTER_MODEL", _DEFAULT_OPENROUTER_MODEL
            )
            providers.append(
                AuthProvider(
                    name="openrouter",
                    base_url=_DEFAULT_OPENROUTER_URL,
                    model_name=or_model,
                    api_key=or_key,
                    supports_embeddings=False,
                )
            )

        # 3. Локальная Ollama — всегда финальный fallback
        local_url = os.environ.get("OLLAMA_BASE_URL", _DEFAULT_OLLAMA_URL)
        providers.append(
            AuthProvider(
                name="ollama-local",
                base_url=local_url,
                model_name=default_model,
                api_key=ollama_api_key,
                supports_embeddings=True,
            )
        )

        self._providers = providers

        # Собрать все базовые URL в ротатор (для тех случаев, когда нужен
        # round-robin по эндпоинтам, как в Rust-версии OllamaProvider)
        urls = [p.base_url for p in providers]
        if len(urls) >= 2:
            self._rotator = CredentialRotator(urls)
        else:
            self._rotator = None

        logger.info(
            "AuthManager: loaded %d provider(s): %s",
            len(providers),
            [p.name for p in providers],
        )


# ---------------------------------------------------------------------------
# CLI: вывод таблицы настроенных провайдеров
# ---------------------------------------------------------------------------


def _print_provider_table(providers: List[AuthProvider]) -> None:
    """Вывести таблицу провайдеров в stdout."""
    if not providers:
        print("Нет настроенных провайдеров.")
        return

    # Заголовки
    headers = ["#", "Name", "Base URL", "Model", "API Key", "Embeddings"]
    col_widths = [3, 18, 42, 42, 10, 10]

    def fmt_row(values: List[str]) -> str:
        parts = []
        for val, w in zip(values, col_widths):
            parts.append(f"{val:<{w}}")
        return " | ".join(parts)

    sep = "-+-".join("-" * w for w in col_widths)

    print(fmt_row(headers))
    print(sep)

    for i, p in enumerate(providers):
        key_display = "***" if p.api_key else "—"
        emb_display = "да" if p.supports_embeddings else "нет"
        print(
            fmt_row(
                [
                    str(i + 1),
                    p.name,
                    p.base_url,
                    p.model_name,
                    key_display,
                    emb_display,
                ]
            )
        )

    print(f"\nВсего провайдеров: {len(providers)}")


def main() -> None:
    """Точка входа при запуске модуля как __main__."""
    logging.basicConfig(
        level=logging.INFO,
        format="%(levelname)s | %(message)s",
    )

    manager = AuthManager()
    providers = manager.list_providers()

    print()
    print("=" * 70)
    print("  AI Agent — конфигурация провайдеров")
    print("=" * 70)
    print()

    _print_provider_table(providers)

    rotator = manager.get_rotator()
    if rotator:
        print(f"CredentialRotator активен: {len(rotator)} endpoint(-a/-ов)")
        print(f"  Endpoints: {rotator.endpoints}")
    else:
        print("CredentialRotator неактивен (менее 2 эндпоинтов).")


if __name__ == "__main__":
    main()
