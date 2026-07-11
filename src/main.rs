use ai_agent::agent::Agent;
use ai_agent::provider::OllamaProvider;
use ai_agent::tool_routing::mcp_transport::load_mcp_config;
use ai_agent::types::*;

use std::io::Write;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // ---- MCP конфигурация (опционально) ------------------------------------
    if let Ok(containers) = load_mcp_config("mcp_containers.json") {
        tracing::info!("Loaded {} MCP container(s) from config", containers.len());
        // Регистрация будет выполнена после создания агента
    } else {
        tracing::info!("No MCP config found — skipping MCP tool discovery");
    }

    // ---- Agent demo --------------------------------------------------------
    let ollama = OllamaProvider::local();
    let mut agent = Agent::new(ollama);

    // Регистрируем MCP-тулы (если есть конфиг)
    if let Ok(containers) = load_mcp_config("mcp_containers.json") {
        agent.register_mcp_tools(&containers).await;
    }

    // Добавляем системный промпт
    agent.context.push(Message::new(
        Role::System,
        "You are a helpful assistant with access to tools. Use the 'dummy' tool to demonstrate tool calling when asked.",
    ));

    // Диалоговый цикл
    let model = "qwen2.5:3b";

    loop {
        // Читаем ввод пользователя
        print!("\n> ");
        std::io::stdout().flush()?;

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        let input = input.trim().to_string();

        if input.is_empty() {
            continue;
        }

        if input == "/exit" || input == "/quit" {
            break;
        }

        // Добавляем сообщение пользователя в контекст
        agent.context.push(Message::new(Role::User, &input));

        // Запускаем полный цикл агента
        match agent.run(model).await {
            Ok(response) => {
                println!("\nAssistant: {response}");
            }
            Err(e) => {
                eprintln!("\nError: {e}");
                // При UserAbort можно продолжить диалог
                if matches!(&e, ai_agent::agent::AgentError::UserAbort) {
                    continue;
                }
                break;
            }
        }
    }

    // ---- Branching demo (наследие) -----------------------------------------
    println!("\n--- Branching demo ---");
    let ctx = &mut agent.context;
    ctx.push(Message::new(Role::User, "What is the capital of France?"));
    ctx.create_branch("research");
    ctx.push(Message::new(Role::Assistant, "Paris."));
    println!(
        "Branch '{}' has {} messages (forked from main).",
        ctx.current_branch().name,
        ctx.current_messages().len()
    );

    ctx.switch_by_name("main").unwrap();
    println!(
        "Switched to '{}' — {} messages (original preserved).",
        ctx.current_branch().name,
        ctx.current_messages().len()
    );

    Ok(())
}
