// ---------------------------------------------------------------------------
// AI Agent — Interactive CLI + Frontend WebSocket server
// ---------------------------------------------------------------------------
// Команды:
//   /exit       — выход (с сохранением snapshot)
//   /help       — справка
//   /branch     — список веток контекста
//   /switch <n> — переключиться на ветку по имени
//   /rename <n> — переименовать текущую ветку
//   /tools      — список зарегистрированных тулов
//   /snapshot   — показать снапшот всех веток
//   Ctrl+C      — graceful shutdown (snapshot + завершение)
// ---------------------------------------------------------------------------

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use ai_agent::agent::Agent;
use ai_agent::config::SystemConfig;
use ai_agent::orchestrator::AgentCluster;
use ai_agent::provider::FallbackProvider;
use ai_agent::tool_routing::frontend::{start_frontend_server, ClientCommand, FrontendEvent, FrontendNotifierHook};
use ai_agent::tool_routing::mcp_transport::load_mcp_config;
use ai_agent::types::*;
use tokio::io::AsyncBufReadExt;

/// Сохранить снапшот всех веток в `history_dump.json`.
fn persist_snapshot(agent: &Agent<FallbackProvider>) {
    let snap = agent.context.snapshot();
    match serde_json::to_string_pretty(&snap) {
        Ok(json) => {
            if let Err(e) = std::fs::write("history_dump.json", &json) {
                eprintln!("[WARN] Failed to write history_dump.json: {e}");
            } else {
                tracing::info!("Context snapshot saved to history_dump.json");
            }
        }
        Err(e) => {
            eprintln!("[WARN] Failed to serialize context snapshot: {e}");
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // ---- --config flag: interactive config menu ---------------------------
    if std::env::args().any(|arg| arg == "--config" || arg == "-c") {
        ai_agent::config::run_config_menu();
        std::process::exit(0);
    }

    // ---- Трейсинг: форматированный вывод в stderr, чтобы stdout оставался чистым ----
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // ---- MCP конфигурация (опционально) ------------------------------------
    let mcp_containers = load_mcp_config("mcp_containers.json").ok();
    if let Some(ref containers) = mcp_containers {
        tracing::info!("Loaded {} MCP container(s) from config", containers.len());
    } else {
        tracing::info!("No MCP config found — skipping MCP tool discovery");
    }

    // ---- Загрузка конфигурации из agent_config.json -----------------------
    let system_cfg = SystemConfig::load();

    // ---- Инициализация FallbackProvider ------------------------------------
    // Пул из конфигурации; env-переменные AGENT_PROVIDER_POOL имеют приоритет
    let providers = if std::env::var("AGENT_PROVIDER_POOL").is_ok() {
        tracing::info!("AGENT_PROVIDER_POOL set — overriding config file pool");
        ai_agent::provider::build_provider_pool()
    } else {
        system_cfg.provider_pool.clone()
    };
    let provider = FallbackProvider::new(providers, Duration::from_secs(10));
    tracing::info!(
        "Initialized FallbackProvider with {} providers",
        provider.provider_count(),
    );
    let mut agent = Agent::new(provider);

    // Применяем настройки из конфигурации
    agent.max_steps = system_cfg.max_steps_limit;
    agent.compaction_config.max_messages = system_cfg.token_compaction_threshold;
    agent.safety_auto_approve = system_cfg.safety_auto_approve;

    if system_cfg.safety_auto_approve {
        tracing::info!("Safety auto-approve is ON");
    }

    // Регистрируем MCP-тулы (если есть конфиг)
    if let Some(ref containers) = mcp_containers {
        agent.register_mcp_tools(containers).await;
    }

    // Системный промпт
    agent.context.push(Message::new(
        Role::System,
        "You are a helpful assistant with access to file system tools: \
         read_file, write_file, glob, grep. You can read, create, and modify \
         files in the project directory.",
    ));

    // ---- Запуск фронтенд-сервера (WebSocket + статика, 0.0.0.0:8080) ------
    let (frontend_tx, frontend_shutdown_tx, cmd_rx, safety_cmd_rx) = start_frontend_server();
    let notifier_hook = Arc::new(FrontendNotifierHook::new(frontend_tx.clone()));
    agent.add_post_hook(notifier_hook);
    agent.set_frontend_tx(frontend_tx.clone());
    agent.set_safety_approval_rx(safety_cmd_rx);

    let model = std::env::var("AI_AGENT_MODEL").unwrap_or_else(|_| "qwen2.5:3b".into());

    // ---- Приветствие -------------------------------------------------------
    println!();
    println!("╔══════════════════════════════════════════════╗");
    println!("║         AI Agent — Interactive CLI           ║");
    println!("╠══════════════════════════════════════════════╣");
    println!("║  Model: {:<33} ║", model);
    println!("║  Tools: {:<33} ║", agent.router.len());
    println!("║  Branch: {:<31} ║", agent.context.current_branch().name);
    println!("║  Messages: {:<29} ║", agent.context.current_messages().len());
    println!("║  UI: http://127.0.0.1:8080                ║");
    println!("╠══════════════════════════════════════════════╣");
    println!("║  /help — список команд                      ║");
    println!("╚══════════════════════════════════════════════╝");
    println!();

    // ---- Диалоговый цикл (async stdin + ctrl_c + WebSocket-команды) -------
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut line_buf = String::new();
    let mut shutdown_requested = false;
    let mut cmd_rx = cmd_rx; // ensure mut for recv

    loop {
        line_buf.clear();

        // Промпт
        print!("> ");
        std::io::stdout().flush()?;

        // ---- select: stdin / WebSocket-команды / Ctrl+C -----------------
        let input = tokio::select! {
            result = stdin.read_line(&mut line_buf) => {
                match result {
                    Ok(0) => {
                        // EOF (Ctrl+D)
                        println!();
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        eprintln!("[ERROR] stdin error: {e}");
                        break;
                    }
                }
                line_buf.trim().to_string()
            }
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(ClientCommand::StartTask { prompt }) => {
                        println!("\n[UI] New task: {prompt}");
                        prompt
                    }
                    Some(ClientCommand::SwitchBranch { name }) => {
                        match agent.context.switch_by_name(&name) {
                            Ok(()) => {
                                tracing::info!("Switched to branch '{name}'");
                                println!("\n  Switched to branch '{name}'\n");
                            }
                            Err(e) => {
                                tracing::warn!("Branch switch failed: {e}");
                                println!("\n  {e}\n");
                            }
                        }
                        continue;
                    }
                    Some(ClientCommand::SafetyResponse { .. }) => {
                        // SafetyResponse направляется отдельным каналом в агент
                        continue;
                    }
                    None => {
                        // Канал закрыт — сервер остановлен
                        continue;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                println!("\n[INFO] Ctrl+C received — shutting down gracefully...");
                shutdown_requested = true;
                break;
            }
        };

        if input.is_empty() {
            continue;
        }

        // ── Встроенные команды ──────────────────────────────────────────
        if input.starts_with('/') {
            match input.as_str() {
                "/exit" | "/quit" => {
                    shutdown_requested = true;
                    break;
                }
                "/help" => {
                    println!();
                    println!("  Commands:");
                    println!("    /exit              Exit the agent");
                    println!("    /help              Show this help");
                    println!("    /branch            List all context branches");
                    println!("    /switch <name>     Switch to a branch by name");
                    println!("    /rename <name>     Rename current branch");
                    println!("    /tools             List registered tools");
                    println!("    /snapshot          Show snapshot of all branches");
                    println!("    /swarm             Run parallel sub-agents (demo)");
                    println!();
                    println!("  Everything else is sent to the LLM as a user message.");
                    println!();
                }
                "/branch" => {
                    let current = agent.context.current_branch().name.clone();
                    println!();
                    for b in agent.context.list() {
                        let marker = if b.name == current { " *" } else { "  " };
                        println!("  {}{} ({})", marker, b.name, b.messages.len());
                    }
                    println!();
                }
                "/tools" => {
                    println!();
                    let names = agent.router.tool_names();
                    if names.is_empty() {
                        println!("  No tools registered.");
                    } else {
                        println!("  Registered tools ({}):", names.len());
                        for name in names {
                            println!("    - {name}");
                        }
                    }
                    println!();
                }
                "/snapshot" => {
                    let snap = agent.context.snapshot();
                    println!();
                    for (id, (name, msgs)) in &snap {
                        let current = agent.context.current_branch().name.as_str();
                        let marker = if name.as_str() == current {
                            " *"
                        } else {
                            "  "
                        };
                        println!("  {}{} [{}] ({} msgs)", marker, name, &id[..8], msgs.len());
                    }
                    println!();
                }
                "/swarm" => {
                    let provider = agent.provider.clone();
                    let mut cluster = AgentCluster::new(provider);
                    // Copy current context messages
                    for msg in agent.context.current_messages().iter() {
                        cluster.context.push(msg.clone());
                    }

                    println!();
                    println!("  Spawning 2 parallel sub-agents...");

                    let tasks = vec![
                        (
                            "researcher".to_string(),
                            "Use the glob tool to list all .rs files in the src directory. \
                             Report the filenames you find."
                                .to_string(),
                        ),
                        (
                            "summarizer".to_string(),
                            "Read the Cargo.toml file and summarize its dependencies. \
                             List the key dependencies and their purposes."
                                .to_string(),
                        ),
                    ];

                    match cluster.execute_parallel_tasks(tasks, &model).await {
                        Ok(report) => {
                            println!();
                            println!("{}", report);
                            println!();
                        }
                        Err(e) => {
                            eprintln!("  Swarm execution error: {e}");
                        }
                    }
                }
                _ => {
                    // Парсим команды с аргументами: /switch <name>, /rename <name>
                    let trimmed = input.trim_start_matches('/');
                    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
                    match parts[0] {
                        "switch" => {
                            let name = parts.get(1).unwrap_or(&"");
                            if name.is_empty() {
                                println!("  Usage: /switch <branch_name>");
                            } else {
                                match agent.context.switch_by_name(name) {
                                    Ok(()) => {
                                        tracing::info!("Switched to branch '{name}'");
                                        println!("  Switched to branch '{name}'");
                                    }
                                    Err(e) => {
                                        println!("  {e}");
                                    }
                                }
                            }
                        }
                        "rename" => {
                            let name = parts.get(1).unwrap_or(&"");
                            if name.is_empty() {
                                println!("  Usage: /rename <new_name>");
                            } else {
                                agent.context.rename(name);
                                tracing::info!("Branch renamed to '{name}'");
                                println!("  Branch renamed to '{name}'");
                            }
                        }
                        other => {
                            println!("  Unknown command: /{other}");
                            println!("  Type /help for available commands.");
                        }
                    }
                }
            }
            continue;
        }

        // ── Сообщение пользователю ──────────────────────────────────────
        agent.context.push(Message::new(Role::User, &input));

        // ── Запуск агента ───────────────────────────────────────────────
        match agent.run(model.as_str()).await {
            Ok(response) => {
                let _ = frontend_tx.send(FrontendEvent::AgentMessage {
                    content: response.clone(),
                });
                println!("\n  Assistant: {response}\n");
            }
            Err(e) => {
                let err_msg = format!("{e}");
                let _ = frontend_tx.send(FrontendEvent::AgentMessage {
                    content: format!("⚠️ Error: {err_msg}"),
                });
                eprintln!("Error: {e}");
                if matches!(&e, ai_agent::agent::AgentError::UserAbort) {
                    continue;
                }
                break;
            }
        }
    }

    // ---- Персистентный снапшот на выходе ----------------------------------
    if shutdown_requested {
        persist_snapshot(&agent);
    }

    // ---- Остановка фронтенд-сервера ---------------------------------------
    let _ = frontend_shutdown_tx.send(true);
    tracing::info!("Frontend server shut down");

    println!("Goodbye!");
    Ok(())
}
