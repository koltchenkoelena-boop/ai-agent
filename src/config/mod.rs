// ---------------------------------------------------------------------------
// Interactive CLI Configuration Module (Phase 6)
//
//   SystemConfig  — сериализуемая структура настроек для agent_config.json
//   run_config_menu() — интерактивное терминальное меню на dialoguer
// ---------------------------------------------------------------------------

use std::path::PathBuf;

use dialoguer::{Input, Select};

use crate::provider::{ProviderConfig, ProviderKind};

// ---------------------------------------------------------------------------
// Конфигурация системы
// ---------------------------------------------------------------------------

/// Полная конфигурация агента, сохраняемая в `agent_config.json`.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct SystemConfig {
    /// Пул провайдеров LLM.
    pub provider_pool: Vec<ProviderConfig>,
    /// Максимальное количество шагов в цикле `Agent::run()`.
    /// Используется как `max_steps` в `Agent`.
    pub max_steps_limit: usize,
    /// Порог компакции контекста (максимум сообщений до триггера).
    /// Передаётся в `CompactionConfig::max_messages`.
    pub token_compaction_threshold: usize,
    /// Автоматически подтверждать все вызовы инструментов (обходить Safety).
    pub safety_auto_approve: bool,
}

impl Default for SystemConfig {
    fn default() -> Self {
        Self {
            provider_pool: vec![ProviderConfig {
                name: "ollama-local".into(),
                base_url: std::env::var("OLLAMA_BASE_URL")
                    .unwrap_or_else(|_| "http://localhost:11434".into()),
                model_name: std::env::var("AI_AGENT_MODEL")
                    .unwrap_or_else(|_| "qwen2.5-coder:7b".into()),
                api_key: std::env::var("OLLAMA_API_KEY").ok().filter(|k| !k.is_empty()),
                supports_embeddings: true,
                kind: ProviderKind::OpenAI,
            }],
            max_steps_limit: 0,
            token_compaction_threshold: 15,
            safety_auto_approve: false,
        }
    }
}

/// Путь к файлу конфигурации по умолчанию.
fn config_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("agent_config.json")
}

impl SystemConfig {
    /// Загрузить конфигурацию из `agent_config.json`.
    ///
    /// Если файла нет — возвращает `Default` (локальная Ollama).
    /// Если файл повреждён — логирует предупреждение и возвращает `Default`.
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(json) => match serde_json::from_str(&json) {
                Ok(cfg) => {
                    tracing::info!("Loaded config from {}", path.display());
                    return cfg;
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse {}: {e} — using defaults",
                        path.display()
                    );
                }
            },
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("Failed to read {}: {e}", path.display());
                }
            }
        }
        let cfg = Self::default();
        tracing::info!("Using default config ({} not found)", path.display());
        cfg
    }

    /// Сохранить конфигурацию в `agent_config.json`.
    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let path = config_path();
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json)?;
        tracing::info!("Config saved to {}", path.display());
        println!("\n  ✓ Config saved to {}", path.display());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Interactive CLI menu
// ---------------------------------------------------------------------------

/// Запустить интерактивное меню настройки конфигурации.
///
/// Вызывается при запуске с флагом `--config` / `-c`.
/// После выбора «Сохранить и выйти» записывает `agent_config.json` на диск.
pub fn run_config_menu() {
    let mut cfg = match SystemConfig::load() {
        c => c,
    };

    loop {
        let items = vec![
            "Просмотр текущей конфигурации",
            "Настройка пула провайдеров",
            "Изменение лимитов безопасности",
            "Сохранить и выйти",
        ];

        let choice = Select::new()
            .with_prompt("Меню конфигурации")
            .items(&items)
            .default(0)
            .interact()
            .unwrap_or(3);

        match choice {
            0 => view_config(&cfg),
            1 => edit_providers(&mut cfg),
            2 => edit_limits(&mut cfg),
            3 => {
                if cfg.save().is_err() {
                    eprintln!("\n  ✗ Failed to save config");
                }
                break;
            }
            _ => break,
        }
    }
}

/// Пункт 1: показать текущую конфигурацию.
fn view_config(cfg: &SystemConfig) {
    println!();
    println!("  ╔══════════════════════════════════════════════╗");
    println!("  ║         Текущая конфигурация                 ║");
    println!("  ╠══════════════════════════════════════════════╣");
    println!("  ║  Провайдеры ({}):", cfg.provider_pool.len());
    for (i, p) in cfg.provider_pool.iter().enumerate() {
        let masked_key = p.api_key.as_deref().map(|k| {
            if k.len() > 8 {
                format!("{}…", &k[..8])
            } else {
                "****".into()
            }
        }).unwrap_or_else(|| "—".into());
        println!("  ║    {}. {} ({})", i + 1, p.name, p.model_name);
        println!("  ║        URL: {}", p.base_url);
        println!("  ║        Key: {}", masked_key);
    }
    println!("  ║");
    println!("  ║  Max steps:         {}", cfg.max_steps_limit);
    println!("  ║  Compaction порог:  {}", cfg.token_compaction_threshold);
    println!("  ║  Safety auto-approve: {}", if cfg.safety_auto_approve { "✓" } else { "—" });
    println!("  ╚══════════════════════════════════════════════╝");
    println!();
    pause();
}

/// Пункт 2: редактировать пул провайдеров.
fn edit_providers(cfg: &mut SystemConfig) {
    loop {
        println!();
        let mut items: Vec<String> = vec!["+ Добавить провайдера".into()];
        for (i, p) in cfg.provider_pool.iter().enumerate() {
            items.push(format!("{}. {} ({})", i + 1, p.name, p.model_name));
        }
        items.push("← Назад".into());

        let choice = Select::new()
            .with_prompt("Пул провайдеров")
            .items(&items)
            .default(0)
            .interact()
            .unwrap_or(items.len() - 1);

        if choice == 0 {
            // Добавить нового провайдера
            let name: String = Input::new()
                .with_prompt("Имя провайдера")
                .default("my-provider".into())
                .interact_text()
                .unwrap_or_else(|_| "my-provider".into());

            let base_url: String = Input::new()
                .with_prompt("Base URL")
                .default("http://localhost:11434".into())
                .interact_text()
                .unwrap_or_else(|_| "http://localhost:11434".into());

            let model_name: String = Input::new()
                .with_prompt("Model name")
                .default("qwen2.5-coder:7b".into())
                .interact_text()
                .unwrap_or_else(|_| "qwen2.5-coder:7b".into());

            let api_key: String = Input::new()
                .with_prompt("API-ключ (Enter = без ключа)")
                .allow_empty(true)
                .interact_text()
                .unwrap_or_default();

            let supports_embeddings = {
                let embed_items = vec!["Да", "Нет"];
                let idx = Select::new()
                    .with_prompt("Поддерживает эмбеддинги?")
                    .items(&embed_items)
                    .default(1)
                    .interact()
                    .unwrap_or(1);
                idx == 0
            };

            cfg.provider_pool.push(ProviderConfig {
                name,
                base_url,
                model_name,
                api_key: if api_key.is_empty() { None } else { Some(api_key) },
                supports_embeddings,
                kind: ProviderKind::OpenAI,
            });
            println!("\n  ✓ Провайдер добавлен");
            pause();
        } else if choice < items.len() - 1 {
            // Редактировать существующего
            let idx = choice - 1;
            edit_single_provider(&mut cfg.provider_pool[idx]);
        } else {
            break;
        }
    }
}

/// Редактировать одного провайдера.
fn edit_single_provider(p: &mut ProviderConfig) {
    loop {
        let items = vec![
            format!("1. Имя:        {}", p.name),
            format!("2. Base URL:   {}", p.base_url),
            format!("3. Model name: {}", p.model_name),
            format!(
                "4. API-ключ:   {}",
                p.api_key.as_deref().map(|k| {
                    if k.len() > 8 { format!("{}…", &k[..8]) } else { "****".into() }
                }).unwrap_or_else(|| "—".into())
            ),
            format!("5. Embeddings: {}", if p.supports_embeddings { "да" } else { "нет" }),
            "← Назад".into(),
        ];

        let choice = Select::new()
            .with_prompt("Редактирование провайдера")
            .items(&items)
            .default(0)
            .interact()
            .unwrap_or(5);

        match choice {
            0 => {
                let val: String = Input::new()
                    .with_prompt("Новое имя")
                    .default(p.name.clone())
                    .interact_text()
                    .unwrap_or_else(|_| p.name.clone());
                p.name = val;
            }
            1 => {
                let val: String = Input::new()
                    .with_prompt("Новый Base URL")
                    .default(p.base_url.clone())
                    .interact_text()
                    .unwrap_or_else(|_| p.base_url.clone());
                p.base_url = val;
            }
            2 => {
                let val: String = Input::new()
                    .with_prompt("Новая модель")
                    .default(p.model_name.clone())
                    .interact_text()
                    .unwrap_or_else(|_| p.model_name.clone());
                p.model_name = val;
            }
            3 => {
                let val: String = Input::new()
                    .with_prompt("Новый API-ключ (Enter = удалить)")
                    .allow_empty(true)
                    .interact_text()
                    .unwrap_or_default();
                p.api_key = if val.is_empty() { None } else { Some(val) };
            }
            4 => {
                let embed_items = vec!["Да", "Нет"];
                let idx = Select::new()
                    .with_prompt("Поддерживает эмбеддинги?")
                    .items(&embed_items)
                    .default(if p.supports_embeddings { 0 } else { 1 })
                    .interact()
                    .unwrap_or(0);
                p.supports_embeddings = idx == 0;
            }
            _ => break,
        }
    }
}

/// Пункт 3: редактировать лимиты безопасности.
fn edit_limits(cfg: &mut SystemConfig) {
    loop {
        let items = vec![
            format!(
                "1. Max steps limit (0 = безлимитно): {}",
                cfg.max_steps_limit
            ),
            format!(
                "2. Compaction порог (макс. сообщений): {}",
                cfg.token_compaction_threshold
            ),
            format!("3. Safety auto-approve: {}", if cfg.safety_auto_approve { "✓" } else { "—" }),
            "← Назад".into(),
        ];

        let choice = Select::new()
            .with_prompt("Лимиты безопасности")
            .items(&items)
            .default(0)
            .interact()
            .unwrap_or(3);

        match choice {
            0 => {
                let val: String = Input::new()
                    .with_prompt("Max steps (0 = безлимитно)")
                    .default(cfg.max_steps_limit.to_string())
                    .validate_with(|input: &String| -> Result<(), &str> {
                        input
                            .parse::<usize>()
                            .map(|_| ())
                            .map_err(|_| "Введите целое число")
                    })
                    .interact_text()
                    .unwrap_or_else(|_| cfg.max_steps_limit.to_string());
                if let Ok(n) = val.parse::<usize>() {
                    cfg.max_steps_limit = n;
                }
            }
            1 => {
                let val: String = Input::new()
                    .with_prompt("Compaction порог (минимум 5)")
                    .default(cfg.token_compaction_threshold.to_string())
                    .validate_with(|input: &String| -> Result<(), &str> {
                        input
                            .parse::<usize>()
                            .map(|_| ())
                            .map_err(|_| "Введите целое число")
                    })
                    .interact_text()
                    .unwrap_or_else(|_| cfg.token_compaction_threshold.to_string());
                if let Ok(n) = val.parse::<usize>() {
                    cfg.token_compaction_threshold = n.max(5);
                }
            }
            2 => {
                cfg.safety_auto_approve = !cfg.safety_auto_approve;
            }
            _ => break,
        }
    }
}

/// Небольшая пауза с ожиданием Enter.
fn pause() {
    println!("  Нажмите Enter для продолжения...");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}
