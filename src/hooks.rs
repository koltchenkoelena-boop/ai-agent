// ---------------------------------------------------------------------------
// Hooks — система расширений для перехвата вызовов инструментов
//
//   PreToolHook   → блокирующий хук до ToolRouter (может модифицировать/отменить)
//   PostToolHook  → фоновый хук после execute (логирование, метрики, память)
// ---------------------------------------------------------------------------

use async_trait::async_trait;

use crate::types::{Message, ToolCall};

// ---------------------------------------------------------------------------
// PreToolHook
// ---------------------------------------------------------------------------

/// Блокирующий хук, вызываемый после Safety Pipeline, но до передачи
/// в `ToolRouter`. Может модифицировать `ToolCall` (например, изменить
/// аргументы) или отменить вызов, вернув `Err(reason)`.
#[async_trait]
pub trait PreToolHook: Send + Sync {
    /// Вызывается перед выполнением инструмента.
    ///
    /// * `call` — мутабельная ссылка на `ToolCall`, позволяет изменить
    ///   имя тула или аргументы.
    /// * `context` — текущая история диалога (только чтение).
    ///
    /// Возвращает `Ok(())` для продолжения или `Err(reason)` для отмены.
    async fn on_pre_use(
        &self,
        call: &mut ToolCall,
        context: &[Message],
    ) -> Result<(), String>;
}

// ---------------------------------------------------------------------------
// PostToolHook
// ---------------------------------------------------------------------------

/// Фоновый (fire-and-forget) хук, вызываемый после успешного выполнения
/// инструмента. Не должен блокировать основной цикл агента — каждый вызов
/// оборачивается в `tokio::spawn`.
#[async_trait]
pub trait PostToolHook: Send + Sync {
    /// Вызывается после выполнения инструмента.
    ///
    /// * `call` — исходный `ToolCall` (каким он был отправлен в `ToolRouter`).
    /// * `result` — результат выполнения (`Ok(text)` или `Err(text)`).
    /// * `context` — текущая история диалога (только чтение).
    async fn on_post_use(
        &self,
        call: &ToolCall,
        result: &Result<String, String>,
        context: &[Message],
    );
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::*;

    // -----------------------------------------------------------------------
    // Mock PreToolHook — логирует вызовы и может модифицировать аргументы
    // -----------------------------------------------------------------------

    struct AppendArgHook {
        suffix: String,
    }

    #[async_trait]
    impl PreToolHook for AppendArgHook {
        async fn on_pre_use(
            &self,
            call: &mut ToolCall,
            _context: &[Message],
        ) -> Result<(), String> {
            // Добавляем суффикс к аргументам
            let mut args: serde_json::Value =
                serde_json::from_str(&call.function.arguments).unwrap_or(serde_json::json!({}));
            if let Some(obj) = args.as_object_mut() {
                obj.insert("hook_applied".into(), serde_json::json!(true));
                obj.insert("suffix".into(), serde_json::json!(self.suffix));
            }
            call.function.arguments = args.to_string();
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_pre_hook_modifies_arguments() {
        let hook = AppendArgHook {
            suffix: "_via_hook".into(),
        };

        let mut call = ToolCall {
            id: "c1".into(),
            r#type: "function".into(),
            function: FunctionCall {
                name: "dummy".into(),
                arguments: r#"{"input":"test"}"#.into(),
            },
        };

        let ctx = vec![Message::new(Role::User, "do something")];

        let result = hook.on_pre_use(&mut call, &ctx).await;
        assert!(result.is_ok());

        // Проверяем, что аргументы были модифицированы
        let parsed: serde_json::Value = serde_json::from_str(&call.function.arguments).unwrap();
        assert_eq!(parsed["hook_applied"], serde_json::json!(true));
        assert_eq!(parsed["suffix"], serde_json::json!("_via_hook"));
        // Оригинальное поле сохранилось
        assert_eq!(parsed["input"], serde_json::json!("test"));
    }

    // -----------------------------------------------------------------------
    // Mock PostToolHook — проверяем, что вызывается с корректными данными
    // -----------------------------------------------------------------------

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    struct LoggingHook {
        called: Arc<AtomicBool>,
    }

    #[async_trait]
    impl PostToolHook for LoggingHook {
        async fn on_post_use(
            &self,
            _call: &ToolCall,
            result: &Result<String, String>,
            _context: &[Message],
        ) {
            // В реальном хуке здесь было бы логирование, метрики и т.д.
            if result.is_ok() {
                self.called.store(true, Ordering::SeqCst);
            }
        }
    }

    #[tokio::test]
    async fn test_post_hook_fires_in_background() {
        let flag = Arc::new(AtomicBool::new(false));
        let hook = LoggingHook {
            called: flag.clone(),
        };

        let call = ToolCall {
            id: "c2".into(),
            r#type: "function".into(),
            function: FunctionCall {
                name: "dummy".into(),
                arguments: r#"{}"#.into(),
            },
        };

        let result: Result<String, String> = Ok("success".into());
        let ctx = vec![Message::new(Role::User, "test")];

        // Запускаем хук в отдельном таске (как это делает Agent)
        let handle = tokio::spawn(async move {
            hook.on_post_use(&call, &result, &ctx).await;
        });

        handle.await.unwrap();

        // Проверяем, что хук отработал
        assert!(flag.load(Ordering::SeqCst));
    }
}
