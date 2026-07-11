// ---------------------------------------------------------------------------
// Orchestrator — кластерное исполнение задач через параллельных суб-агентов
//
//   AgentCluster<P>  → владеет провайдером + контекстом
//   execute_parallel_tasks → веерное разветвление: N задач → N суб-агентов
//                              → сборка результатов → merge веток
// ---------------------------------------------------------------------------

use futures_util::future::join_all;

use crate::agent::{Agent, AgentError};
use crate::context::{ContextManager, MergeStrategy};
use crate::provider::ModelProvider;
use crate::types::*;

// ---------------------------------------------------------------------------
// AgentCluster
// ---------------------------------------------------------------------------

/// Кластер агентов для параллельного выполнения нескольких задач.
///
/// Каждая задача задаётся парой `(имя_агента, промпт)`. Имя агента
/// используется для именования ветки (с дедупликацией).
///
/// # Пример
///
/// ```ignore
/// let mut cluster = AgentCluster::new(ollama_provider);
/// cluster.context.push(Message::new(Role::System, "You are a helpful assistant."));
///
/// let tasks = vec![
///     ("researcher".into(), "List files in src/".into()),
///     ("summarizer".into(), "Read Cargo.toml".into()),
/// ];
///
/// let report = cluster.execute_parallel_tasks(tasks, "qwen2.5:3b").await?;
/// println!("{report}");
/// ```
pub struct AgentCluster<P: ModelProvider> {
    pub provider: P,
    pub context: ContextManager,
}

impl<P: ModelProvider + Clone + 'static> AgentCluster<P> {
    /// Создать новый кластер с заданным провайдером LLM.
    pub fn new(provider: P) -> Self {
        Self {
            provider,
            context: ContextManager::new(),
        }
    }

    /// Выполнить несколько задач параллельно.
    ///
    /// Каждая задача — пара `(имя_агента, промпт)`. Для каждой задачи:
    /// 1. Создаётся суб-агент с копией текущего контекста + промптом задачи.
    /// 2. Все суб-агенты запускаются конкурентно через `futures_util::future::join_all`.
    /// 3. После завершения каждый результат сохраняется в отдельную ветку
    ///    (с именем от `имя_агента`, дедуплицированным при необходимости).
    /// 4. Все ветки сливаются в текущую активную ветку стратегией `Union`.
    ///
    /// Возвращает текстовый отчёт с выводами всех суб-агентов.
    ///
    /// # Errors
    /// Возвращает `AgentError` при фатальных ошибках в родительской обработке
    /// (сами суб-агенты возвращают ошибки в логах, но не прерывают остальные).
    pub async fn execute_parallel_tasks(
        &mut self,
        tasks: Vec<(String, String)>,
        model: &str,
    ) -> Result<String, AgentError> {
        if tasks.is_empty() {
            return Ok("No tasks to execute.".to_string());
        }

        let parent_messages = self.context.current_messages().to_vec();
        let main_name = self.context.current_branch().name.clone();
        let provider = self.provider.clone();
        let model_owned = model.to_string();

        let n_tasks = tasks.len();

        // ---- Спаун суб-агентов -------------------------------------------------
        let handles: Vec<_> = tasks
            .into_iter()
            .enumerate()
            .map(|(i, (agent_name, prompt))| {
                let provider = provider.clone();
                let parent_msgs = parent_messages.clone();
                let model = model_owned.clone();

                tokio::spawn(async move {
                    let mut agent = Agent::new(provider);
                    agent.context.extend(parent_msgs);
                    agent.context.push(Message::new(Role::User, &prompt));

                    let result = agent.run(&model).await;
                    let messages = agent.context.current_messages().to_vec();
                    let branch_name = if agent_name.is_empty() {
                        format!("task-{i}")
                    } else {
                        agent_name.clone()
                    };
                    (result, messages, branch_name)
                })
            })
            .collect();

        // ---- Сбор результатов ---------------------------------------------------
        let results = join_all(handles).await;

        // ---- Сохраняем каждую завершённую ветку в ContextManager ----------------
        let mut branch_names: Vec<String> = Vec::new();
        let mut outputs: Vec<String> = Vec::new();

        for result in results {
            match result {
                Ok((agent_result, branch_msgs, branch_name_hint)) => {
                    // Generate unique branch name
                    let mut unique_name = branch_name_hint.clone();
                    let mut cnt = 0;
                    while self.context.list().iter().any(|b| b.name == unique_name)
                        || branch_names.contains(&unique_name)
                    {
                        cnt += 1;
                        unique_name = format!("{branch_name_hint}_{cnt}");
                    }

                    tracing::info!(
                        "Sub-agent '{}' completed with {} messages",
                        unique_name,
                        branch_msgs.len()
                    );

                    // Пишем агента в ветку
                    self.context.create_branch(&unique_name);
                    self.context.current_branch_mut().messages = branch_msgs;

                    // Формируем вывод в отчёт
                    match &agent_result {
                        Ok(text) => outputs.push(format!("[{}]: {}", unique_name, text)),
                        Err(e) => outputs.push(format!("[{}] ERROR: {}", unique_name, e)),
                    }

                    // Возвращаемся на основную ветку
                    let _ = self.context.switch_by_name(&main_name);
                    branch_names.push(unique_name);
                }
                Err(e) => {
                    tracing::error!("Sub-agent panicked or failed: {e}");
                    outputs.push(format!("[PANIC]: {e}"));
                }
            }
        }

        // ---- Merge всех веток обратно в main ------------------------------------
        for branch_name in &branch_names {
            let branch_id = {
                let branch = match self.context.list().iter().find(|b| b.name == *branch_name) {
                    Some(b) => b.id.clone(),
                    None => continue,
                };
                branch
            };

            if let Err(e) = self.context.merge(&branch_id, MergeStrategy::Union) {
                tracing::warn!("Failed to merge branch '{branch_name}': {e}");
            }

            // Удаляем ветку после успешного merge
            let _ = self.context.delete(&branch_id);
        }

        let report = format!(
            "Parallel execution completed. {}/{} branches merged.\n\
             ───────────────────────────────────────────\n\
             {}",
            branch_names.len(),
            n_tasks,
            outputs.join("\n---\n"),
        );

        tracing::info!(
            "Parallel execution complete: {} tasks merged into '{}'",
            branch_names.len(),
            main_name,
        );

        Ok(report)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crate::provider::{ProviderError, ProviderStream};

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // MockProvider with controlled responses
    #[derive(Clone)]
    struct MockProvider {
        responses: Vec<Vec<ChatChunk>>,
        call_count: Arc<AtomicUsize>,
    }

    impl MockProvider {
        fn new(responses: Vec<Vec<ChatChunk>>) -> Self {
            Self {
                responses,
                call_count: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for MockProvider {
        async fn stream_chat(
            &self,
            _model: &str,
            _messages: Vec<Message>,
            _tools: Option<Vec<ToolDefinition>>,
        ) -> Result<ProviderStream, ProviderError> {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
            let chunks = self.responses.get(idx).cloned().unwrap_or_default();

            let stream = async_stream::try_stream! {
                for chunk in chunks {
                    yield chunk;
                }
            };

            Ok(Box::pin(stream))
        }

        async fn get_embedding(&self, _text: &str) -> Result<Vec<f32>, ProviderError> {
            Ok(vec![0.1, 0.2, 0.3, 0.4])
        }
    }

    #[tokio::test]
    async fn test_parallel_merge_union() {
        // Создаём MockProvider: для каждого суб-агента — один вызов, возвращающий текст
        let provider = MockProvider::new(vec![
            // sub-agent 0: researcher
            vec![ChatChunk {
                delta_content: Some("Found 3 .rs files: main.rs, lib.rs, orchestrator.rs".into()),
                delta_tool_calls: None,
            }],
            // sub-agent 1: summarizer
            vec![ChatChunk {
                delta_content: Some("Cargo.toml declares dependencies: tokio, serde, axum".into()),
                delta_tool_calls: None,
            }],
        ]);

        let mut cluster = AgentCluster::new(provider);
        cluster.context.push(Message::new(Role::System, "You are a helpful assistant."));

        let tasks = vec![
            ("researcher".to_string(), "List .rs files".to_string()),
            ("summarizer".to_string(), "Summarize Cargo.toml".to_string()),
        ];

        let report = cluster
            .execute_parallel_tasks(tasks, "test-model")
            .await
            .expect("Parallel execution failed");

        // Проверяем, что отчёт содержит выводы обоих агентов
        assert!(
            report.contains("Found 3"),
            "Report should contain researcher output, got:\n{report}"
        );
        assert!(
            report.contains("Cargo.toml declares"),
            "Report should contain summarizer output, got:\n{report}"
        );

        // Проверяем, что ветки слились в основную
        let current_msgs = cluster.context.current_messages();
        // Сообщения: system (изначальная) + researcher assistant + summarizer assistant
        // Количество может варьироваться, но должен быть хотя бы один ассистентский ответ
        let assistant_count = current_msgs.iter().filter(|m| m.role == Role::Assistant).count();
        assert!(
            assistant_count >= 2,
            "Expected at least 2 assistant messages after merge, got {assistant_count}"
        );

        // Веток, кроме main, быть не должно (удалены после merge)
        let branches = cluster.context.list();
        assert!(
            branches.iter().all(|b| b.name == "main"),
            "All branches except main should be deleted, got: {:?}",
            branches.iter().map(|b| &b.name).collect::<Vec<_>>(),
        );
    }
}
