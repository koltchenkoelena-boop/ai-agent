// ---------------------------------------------------------------------------
// AI Agent — Interactive CLI
// ---------------------------------------------------------------------------
// Команды:
//   /exit       — выход
//   /help       — справка
//   /branch     — список веток контекста
//   /switch <n> — переключиться на ветку по имени
//   /rename <n> — переименовать текущую ветку
//   /tools      — список зарегистрированных тулов
//   /snapshot   — показать снапшот всех веток
// ---------------------------------------------------------------------------

use std::io::Write;

use ai_agent::agent::Agent;
use ai_agent::provider::OllamaProvider;
use ai_agent::tool_routing::mcp_transport::load_mcp_config;
use ai_agent::types::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    // ---- Инициализация агента ----------------------------------------------
    let ollama = OllamaProvider::local();
    let mut agent = Agent::new(ollama);

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

    let model = std::env::var("AI_AGENT_MODEL").unwrap_or_else(|_| "qwen2.5:3b".into());

    // ---- Приветствие -------------------------------------------------------
    println!();
    println!("╔══════════════════════════════════════════╗");
    println!("║         AI Agent — Interactive CLI       ║");
    println!("╠══════════════════════════════════════════╣");
    println!("║  Model: {:<33} ║", model);
    println!("║  Tools: {:<33} ║", agent.router.len());
    println!("║  Branch: {:<31} ║", agent.context.current_branch().name);
    println!("║  Messages: {:<29} ║", agent.context.current_messages().len());
    println!("╠══════════════════════════════════════════╣");
    println!("║  /help — список команд                  ║");
    println!("╚══════════════════════════════════════════╝");
    println!();

    // ---- Диалоговый цикл ---------------------------------------------------
    loop {
        // Промпт
        print!("> ");
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim().to_string();

        if input.is_empty() {
            continue;
        }

        // ── Встроенные команды ──────────────────────────────────────────
        if input.starts_with('/') {
            match input.as_str() {
                "/exit" | "/quit" => {
                    println!("Goodbye!");
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
                println!("\n  Assistant: {response}\n");
            }
            Err(e) => {
                eprintln!("Error: {e}");
                if matches!(&e, ai_agent::agent::AgentError::UserAbort) {
                    continue;
                }
                break;
            }
        }
    }

    Ok(())
}
