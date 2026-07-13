// ---------------------------------------------------------------------------
// Safety Pipeline — 5 эшелонов защиты, проверяющих ToolCall до исполнения.
//
//   Security    →  инъекции, опасные паттерны
//   Egress      →  (заглушка) проверка исходящих данных
//   Adversary   →  (заглушка) LLM-детектор вредоносных намерений
//   Permission  →  разграничение доступа, User-in-the-loop
//   Repetition  →  защита от зацикливания агента
// ---------------------------------------------------------------------------

use async_trait::async_trait;

use crate::types::{Message, Role, ToolCall};

// ---------------------------------------------------------------------------
// SafetyDecision
// ---------------------------------------------------------------------------

/// Результат проверки эшелона безопасности.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyDecision {
    /// Пропустить выполнение.
    Allow,
    /// Жёсткая блокировка с причиной.
    Deny(String),
    /// Требуется подтверждение пользователя.
    RequiresApproval(String),
}

// ---------------------------------------------------------------------------
// SafetyStage trait
// ---------------------------------------------------------------------------

/// Один эшелон (stage) цепочки безопасности.
#[async_trait]
pub trait SafetyStage: Send + Sync {
    /// Человекочитаемое имя эшелона (для логов).
    fn name(&self) -> &'static str;

    /// Проверить конкретный ToolCall в контексте истории диалога.
    async fn inspect(&self, call: &ToolCall, context: &[Message]) -> SafetyDecision;
}

// ---------------------------------------------------------------------------
// SafetyPipeline
// ---------------------------------------------------------------------------

/// Конвейер эшелонов: все registered stages вызываются последовательно.
/// Первый `Deny` или `RequiresApproval` прерывает проверку.
pub struct SafetyPipeline {
    stages: Vec<Box<dyn SafetyStage>>,
}

impl SafetyPipeline {
    /// Пустой пайплайн (ничего не блокирует).
    pub fn new() -> Self {
        Self { stages: Vec::new() }
    }

    /// Добавить эшелон в конец цепочки.
    pub fn add_stage(&mut self, stage: Box<dyn SafetyStage>) {
        tracing::debug!(stage = stage.name(), "adding safety stage");
        self.stages.push(stage);
    }

    /// Прогнать ToolCall через всю цепочку эшелонов.
    ///
    /// Возвращает:
    /// - `Allow` — все эшелоны пропустили вызов.
    /// - `Deny(reason)` / `RequiresApproval(reason)` — первый заблокировавший.
    pub async fn verify(&self, call: &ToolCall, context: &[Message]) -> SafetyDecision {
        for stage in &self.stages {
            let decision = stage.inspect(call, context).await;
            match decision {
                SafetyDecision::Allow => {
                    tracing::trace!(
                        stage = stage.name(),
                        tool = %call.function.name,
                        tool_args = %call.function.arguments,
                        "allow",
                    );
                    continue;
                }
                SafetyDecision::Deny(ref reason) => {
                    tracing::warn!(
                        stage = stage.name(),
                        tool = %call.function.name,
                        tool_args = %call.function.arguments,
                        reason,
                        "deny",
                    );
                    return decision;
                }
                SafetyDecision::RequiresApproval(ref reason) => {
                    tracing::info!(
                        stage = stage.name(),
                        tool = %call.function.name,
                        tool_args = %call.function.arguments,
                        reason,
                        "requires_approval",
                    );
                    return decision;
                }
            }
        }
        SafetyDecision::Allow
    }

    /// Количество зарегистрированных эшелонов.
    pub fn len(&self) -> usize {
        self.stages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }
}

impl Default for SafetyPipeline {
    fn default() -> Self {
        Self::new()
    }
}

// ===========================================================================
// ЭШЕЛОН 1: SecurityStage — защита от command injection
// ===========================================================================

/// Список имён тулов, при работе с которыми аргументы считаются
/// потенциально опасными (shell-метасимволы вызовут блокировку).
const DANGEROUS_TOOLS: &[&str] = &["execute_bash", "shell", "exec", "run_sh", "cmd", "bash"];

/// Символы, характерные для command chaining / injection.
const INJECTION_PATTERNS: &[&str] = &[";", "&&", "||", "|", "`", "$(", "$VAR", "${"];

/// Блокирует вызовы shell-подобных тулов с подозрительными аргументами.
pub struct SecurityStage;

#[async_trait]
impl SafetyStage for SecurityStage {
    fn name(&self) -> &'static str {
        "security"
    }

    async fn inspect(&self, call: &ToolCall, _context: &[Message]) -> SafetyDecision {
        let tool_name = &call.function.name;

        // Проверяем только опасные тулы
        if !DANGEROUS_TOOLS.contains(&tool_name.as_str()) {
            return SafetyDecision::Allow;
        }

        let args = &call.function.arguments;

        for pattern in INJECTION_PATTERNS {
            if args.contains(pattern) {
                return SafetyDecision::Deny(format!(
                    "command injection marker '{pattern}' detected in args of '{tool_name}'"
                ));
            }
        }

        SafetyDecision::Allow
    }
}

// ===========================================================================
// ЭШЕЛОН 2: (Заглушка) EgressStage — проверка исходящих данных
// ===========================================================================

pub struct EgressStage;

#[async_trait]
impl SafetyStage for EgressStage {
    fn name(&self) -> &'static str {
        "egress"
    }

    async fn inspect(&self, _call: &ToolCall, _context: &[Message]) -> SafetyDecision {
        // TODO: Проверять, не пытается ли агент отправить sensitive данные
        //       наружу (API-ключи, пароли, токены) через сетевые тулы.
        SafetyDecision::Allow
    }
}

// ===========================================================================
// ЭШЕЛОН 3: (Заглушка) AdversaryStage — LLM-детектор вредоносных намерений
// ===========================================================================

pub struct AdversaryStage;

#[async_trait]
impl SafetyStage for AdversaryStage {
    fn name(&self) -> &'static str {
        "adversary_llm"
    }

    async fn inspect(&self, _call: &ToolCall, _context: &[Message]) -> SafetyDecision {
        // TODO: Передать ToolCall + контекст отдельной LLM (например, gemma3:1b)
        //       для классификации: benign / suspicious / malicious.
        SafetyDecision::Allow
    }
}

// ===========================================================================
// ЭШЕЛОН 4: PermissionStage — разграничение доступа (User-in-the-loop)
// ===========================================================================

/// Требует подтверждения от пользователя для критических тулов.
pub struct PermissionStage;

#[async_trait]
impl SafetyStage for PermissionStage {
    fn name(&self) -> &'static str {
        "permission"
    }

    async fn inspect(&self, call: &ToolCall, _context: &[Message]) -> SafetyDecision {
        // Заглушка: запрашиваем подтверждение на execute_bash
        if call.function.name == "execute_bash" {
            return SafetyDecision::RequiresApproval(format!(
                "execute_bash requires user approval (args: {})",
                &call.function.arguments[..call.function.arguments.len().min(120)]
            ));
        }
        SafetyDecision::Allow
    }
}

// ===========================================================================
// ЭШЕЛОН 5: RepetitionStage — защита от зацикливания агента
// ===========================================================================

/// Если агент вызвал один и тот же тул с одинаковыми аргументами
/// N раз подряд (по умолчанию 3) — блокируем, чтобы избежать бесконечного
/// цикла (например, когда тул стабильно возвращает ошибку).
pub struct RepetitionStage {
    /// Максимальное количество повторений до блокировки.
    max_repetitions: usize,
}

impl RepetitionStage {
    pub const fn new(max_repetitions: usize) -> Self {
        Self { max_repetitions }
    }
}

impl Default for RepetitionStage {
    fn default() -> Self {
        Self::new(3)
    }
}

#[async_trait]
impl SafetyStage for RepetitionStage {
    fn name(&self) -> &'static str {
        "repetition"
    }

    async fn inspect(&self, call: &ToolCall, context: &[Message]) -> SafetyDecision {
        let repetition_count =
            count_consecutive_identical_calls(call.function.name.as_str(), &call.function.arguments, context);

        // Блокируем, когда число одинаковых вызовов (включая текущий)
        // достигает max_repetitions.
        // count_consecutive возвращает кол-во из контекста (без текущего),
        // поэтому добавляем 1.
        let total = repetition_count + 1;
        if total >= self.max_repetitions {
            return SafetyDecision::Deny(format!(
                "tool '{}' called with identical arguments {} times in a row (max: {})",
                call.function.name, total, self.max_repetitions,
            ));
        }

        SafetyDecision::Allow
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Сколько раз подряд (до текущего вызова) агент вызывал тот же тул
/// с теми же аргументами. Считаем по assistant-сообщениям с tool_calls.
fn count_consecutive_identical_calls(name: &str, args: &str, context: &[Message]) -> usize {
    let mut count = 0usize;

    // Идём по истории с конца
    for msg in context.iter().rev() {
        if msg.role != Role::Assistant {
            // Любое не-assistant сообщение (User, Tool, System) прерывает серию
            break;
        }
        let Some(tool_calls) = &msg.tool_calls else {
            // Assistant-сообщение без tool_calls прерывает серию
            break;
        };

        // В одном assistant-сообщении может быть несколько ToolCall.
        // Проверяем последний (наиболее вероятный повтор).
        let matched = tool_calls.iter().any(|tc| {
            tc.function.name == name && tc.function.arguments == args
        });

        if matched {
            count += 1;
        } else {
            // Первый ToolCall с другим именем/аргументами прерывает подсчёт
            break;
        }
    }

    count
}

// ===========================================================================
// Factory — собрать пайплайн по умолчанию
// ===========================================================================

/// Собрать `SafetyPipeline` со всеми реализованными эшелонами.
pub fn default_pipeline() -> SafetyPipeline {
    let mut p = SafetyPipeline::new();
    p.add_stage(Box::new(SecurityStage));
    p.add_stage(Box::new(EgressStage));
    p.add_stage(Box::new(AdversaryStage));
    p.add_stage(Box::new(PermissionStage));
    p.add_stage(Box::new(RepetitionStage::default()));
    p
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // SecurityStage
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_security_allows_safe_shell() {
        let stage = SecurityStage;
        let call = ToolCall {
            id: "c1".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "execute_bash".into(),
                arguments: r#"{"cmd":"ls -la"}"#.into(),
            },
        };
        assert_eq!(stage.inspect(&call, &[]).await, SafetyDecision::Allow);
    }

    #[tokio::test]
    async fn test_security_blocks_injection_semicolon() {
        let stage = SecurityStage;
        let call = ToolCall {
            id: "c2".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "execute_bash".into(),
                arguments: r#"{"cmd":"ls; rm -rf /"}"#.into(),
            },
        };
        let decision = stage.inspect(&call, &[]).await;
        assert!(matches!(decision, SafetyDecision::Deny(_)));
    }

    #[tokio::test]
    async fn test_security_blocks_injection_dollar_paren() {
        let stage = SecurityStage;
        let call = ToolCall {
            id: "c3".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "shell".into(),
                arguments: r#"{"cmd":"cat $(ls)"}"#.into(),
            },
        };
        let decision = stage.inspect(&call, &[]).await;
        assert!(matches!(decision, SafetyDecision::Deny(_)));
    }

    #[tokio::test]
    async fn test_security_allows_non_dangerous_tool() {
        let stage = SecurityStage;
        let call = ToolCall {
            id: "c4".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"/tmp/x; rm -rf"}"#.into(),
            },
        };
        // read_file не в списке DANGEROUS_TOOLS — пропускаем
        assert_eq!(stage.inspect(&call, &[]).await, SafetyDecision::Allow);
    }

    // -----------------------------------------------------------------------
    // PermissionStage
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_permission_blocks_execute_bash() {
        let stage = PermissionStage;
        let call = ToolCall {
            id: "c5".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "execute_bash".into(),
                arguments: "{}".into(),
            },
        };
        let decision = stage.inspect(&call, &[]).await;
        assert!(matches!(decision, SafetyDecision::RequiresApproval(_)));
    }

    #[tokio::test]
    async fn test_permission_allows_other() {
        let stage = PermissionStage;
        let call = ToolCall {
            id: "c6".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "read_file".into(),
                arguments: "{}".into(),
            },
        };
        assert_eq!(stage.inspect(&call, &[]).await, SafetyDecision::Allow);
    }

    // -----------------------------------------------------------------------
    // RepetitionStage
    // -----------------------------------------------------------------------

    fn tool_call_msg(tool_name: &str, args: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: None,
            tool_calls: Some(vec![ToolCall {
                id: "ignored".into(),
                r#type: "function".into(),
                function: crate::types::FunctionCall {
                    name: tool_name.into(),
                    arguments: args.into(),
                },
            }]),
            tool_call_id: None,
        }
    }

    #[tokio::test]
    async fn test_repetition_allows_first_call() {
        let stage = RepetitionStage::default();
        let call = ToolCall {
            id: "c7".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "search".into(),
                arguments: r#"{"q":"hello"}"#.into(),
            },
        };
        let ctx = vec![
            Message::new(crate::types::Role::User, "search hello"),
        ];
        assert_eq!(stage.inspect(&call, &ctx).await, SafetyDecision::Allow);
    }

    #[tokio::test]
    async fn test_repetition_blocks_after_3_identical() {
        let stage = RepetitionStage::default();
        let call = ToolCall {
            id: "c10".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "search".into(),
                arguments: r#"{"q":"stuck"}"#.into(),
            },
        };

        // 3 предыдущих вызова с теми же именем и аргументами
        let ctx = vec![
            tool_call_msg("search", r#"{"q":"stuck"}"#),
            tool_call_msg("search", r#"{"q":"stuck"}"#),
            tool_call_msg("search", r#"{"q":"stuck"}"#),
        ];

        let decision = stage.inspect(&call, &ctx).await;
        assert!(matches!(decision, SafetyDecision::Deny(_)));
    }

    #[tokio::test]
    async fn test_repetition_allows_after_name_change() {
        let stage = RepetitionStage::default();
        let call = ToolCall {
            id: "c11".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "search".into(),
                arguments: r#"{"q":"stuck"}"#.into(),
            },
        };

        // 3 вызова но с другим именем между ними
        let ctx = vec![
            tool_call_msg("search", r#"{"q":"stuck"}"#),
            tool_call_msg("other", r#"{}"#),
            tool_call_msg("search", r#"{"q":"stuck"}"#),
        ];

        assert_eq!(stage.inspect(&call, &ctx).await, SafetyDecision::Allow);
    }

    #[tokio::test]
    async fn test_repetition_ignores_user_messages() {
        let stage = RepetitionStage::default();
        let call = ToolCall {
            id: "c12".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "search".into(),
                arguments: r#"{"q":"stuck"}"#.into(),
            },
        };

        let ctx = vec![
            tool_call_msg("search", r#"{"q":"stuck"}"#),
            Message::new(crate::types::Role::User, "continue"),
            tool_call_msg("search", r#"{"q":"stuck"}"#),
            tool_call_msg("search", r#"{"q":"stuck"}"#),
        ];

        // 2 последовательных после user-сообщения + текущий = 3 → блокируем
        let decision = stage.inspect(&call, &ctx).await;
        assert!(matches!(decision, SafetyDecision::Deny(_)));
    }

    // -----------------------------------------------------------------------
    // SafetyPipeline (integration)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_pipeline_allow() {
        let pipeline = default_pipeline();
        let call = ToolCall {
            id: "c20".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "read_file".into(),
                arguments: r#"{"path":"/tmp/test"}"#.into(),
            },
        };
        assert_eq!(pipeline.verify(&call, &[]).await, SafetyDecision::Allow);
    }

    #[tokio::test]
    async fn test_pipeline_security_stops_first() {
        let pipeline = default_pipeline();
        let call = ToolCall {
            id: "c21".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "execute_bash".into(),
                arguments: r#"{"cmd":"ls; rm -rf /"}"#.into(),
            },
        };
        let decision = pipeline.verify(&call, &[]).await;
        assert!(matches!(decision, SafetyDecision::Deny(reason) if reason.contains("injection")));
    }

    #[tokio::test]
    async fn test_pipeline_empty_allows() {
        let pipeline = SafetyPipeline::new();
        let call = ToolCall {
            id: "c22".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "any".into(),
                arguments: "{}".into(),
            },
        };
        assert_eq!(pipeline.verify(&call, &[]).await, SafetyDecision::Allow);
    }

    #[tokio::test]
    async fn test_pipeline_repetition_blocks_after_dangerous_cycle() {
        let pipeline = default_pipeline();
        let call = ToolCall {
            id: "c23".into(),
            r#type: "function".into(),
            function: crate::types::FunctionCall {
                name: "search".into(),
                arguments: r#"{"q":"loop"}"#.into(),
            },
        };

        let ctx = vec![
            tool_call_msg("search", r#"{"q":"loop"}"#),
            tool_call_msg("search", r#"{"q":"loop"}"#),
            tool_call_msg("search", r#"{"q":"loop"}"#),
        ];

        let decision = pipeline.verify(&call, &ctx).await;
        assert!(matches!(decision, SafetyDecision::Deny(reason) if reason.contains("identical arguments")));
    }
}
