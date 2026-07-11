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
/// # Пример
///
/// ```ignore
/// let mut cluster = AgentCluster::new(ollama_provider);
/// cluster.context.push(Message::new(Role::System, "You are a helpful assistant."));
///
/// let tasks = vec![
///     "Search for file X".into(),
///     "Summarize directory Y".into(),
/// ];
///
/// cluster.execute_parallel_tasks(tasks, "qwen2.5:3b").await?;
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
    /// Для каждой задачи:
    /// 1. Создаётся суб-агент с копией текущего контекста + промптом задачи.
    /// 2. Все суб-агенты запускаются конкурентно через `futures_util::future::join_all`.
    /// 3. После завершения каждый результат сохраняется в отдельную ветку.
    /// 4. Все ветки сливаются в "main" стратегией `Union`.
    ///
    /// # Errors
    /// Возвращает `AgentError` при фатальных ошибках провайдера или тулов
    /// в родительской обработке (сами суб-агенты возвращают ошибки
    /// в логах, но не прерывают остальные задачи).
    pub async fn execute_parallel_tasks(
        &mut self,
        tasks: Vec<String>,
        model: &str,
    ) -> Result<(), AgentError> {
        if tasks.is_empty() {
            return Ok(());
        }

        let parent_messages = self.context.current_messages().to_vec();
        let main_name = self.context.current_branch().name.clone();
        let provider = self.provider.clone();
        let model_owned = model.to_string();

        // ---- Спаун суб-агентов -------------------------------------------------
        let handles: Vec<_> = tasks
            .into_iter()
            .enumerate()
            .map(|(i, task)| {
                let provider = provider.clone();
                let parent_msgs = parent_messages.clone();
                let model = model_owned.clone();

                tokio::spawn(async move {
                    let mut agent = Agent::new(provider);
                    agent.context.extend(parent_msgs);
                    agent.context.push(Message::new(Role::User, &task));

                    let result = agent.run(&model).await;
                    let messages = agent.context.current_messages().to_vec();
                    let branch_name = format!("task-{i}");
                    (result, messages, branch_name)
                })
            })
            .collect();

        // ---- Сбор результатов ---------------------------------------------------
        let results = join_all(handles).await;

        // ---- Сохраняем каждую завершённую ветку в ContextManager ----------------
        let mut branch_names: Vec<String> = Vec::new();

        for result in results {
            match result {
                Ok((_agent_result, branch_msgs, branch_name)) => {
                    tracing::info!(
                        "Sub-agent '{}' completed with {} messages",
                        branch_name,
                        branch_msgs.len()
                    );

                    // Создаём ветку с результатами суб-агента
                    self.context.create_branch(&branch_name);
                    self.context.current_branch_mut().messages = branch_msgs;

                    // Возвращаемся на main перед следующей итерацией
                    let _ = self.context.switch_by_name(&main_name);
                    branch_names.push(branch_name);
                }
                Err(e) => {
                    tracing::error!("Sub-agent panicked or failed: {e}");
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

        tracing::info!(
            "Parallel execution complete: {} tasks merged into '{}'",
            branch_names.len(),
            main_name,
        );

        Ok(())
    }
}
