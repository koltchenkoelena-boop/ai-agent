// ---------------------------------------------------------------------------
// ChatLog — JSON-логирование всех этапов пайплайна в NDJSON-файл
// ---------------------------------------------------------------------------
//
// Добавляет второй слой tracing-subscriber, который пишет структурированные
// JSON-события в `chat_logs/YYYY-MM-DD_HH-MM-SS.jsonl`.
//
// Использование в main.rs:
//
//   use tracing_subscriber::layer::SubscriberExt;
//   use tracing_subscriber::Registry;
//
//   let subscriber = Registry::default()
//       .with(tracing_subscriber::fmt::layer().with_env_filter(...))  // stderr
//       .with(ai_agent::chat_log::json_file_layer("chat_logs"));      // JSON-файл
//   tracing::subscriber::set_global_default(subscriber).unwrap();
// ---------------------------------------------------------------------------

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use tracing_subscriber::fmt::MakeWriter;

/// Обёртка над `std::fs::File` с внутренним Mutex для потокобезопасности.
struct ChatLogFile {
    file: Mutex<std::fs::File>,
}

impl Write for ChatLogFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.file.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.file.lock().unwrap().flush()
    }
}

/// `MakeWriter`, который открывает `chat_logs/*.jsonl` в режиме append
/// при каждом обращении. Для CLI-агента это приемлемо.
struct ChatLogMakeWriter {
    path: PathBuf,
}

impl<'a> MakeWriter<'a> for ChatLogMakeWriter {
    type Writer = ChatLogFile;

    fn make_writer(&self) -> Self::Writer {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .unwrap_or_else(|e| panic!("[chat_log] Failed to open '{}': {e}", self.path.display()));
        ChatLogFile {
            file: Mutex::new(file),
        }
    }
}

/// Создать `tracing_subscriber::fmt::Layer` с JSON-форматом, пишущий
/// в файл `chat_logs/<timestamp>.jsonl`.
///
/// Generic по `S` — корректно компонуется в многослойный subscriber
/// (см. main.rs: `.with(stderr_layer).with(json_layer)`).
///
/// ## Пример строки в файле
/// ```json
/// {"timestamp":"2026-07-11T14:30:00.123456+07:00","level":"INFO","fields":{"stage":"llm_call","msg_count":5,"model":"nemotron-3-super"},"target":"ai_agent::agent"}
/// ```
pub fn json_file_layer<S>(
    chat_logs_dir: &str,
) -> impl tracing_subscriber::Layer<S>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    // Создаём директорию, если её нет
    let _ = std::fs::create_dir_all(chat_logs_dir);

    // Имя файла: chat_logs/YYYY-MM-DD_HH-MM-SS.jsonl
    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
    let path = PathBuf::from(chat_logs_dir).join(format!("{timestamp}.jsonl"));

    // Печатаем путь в stderr до инициализации tracing
    let path_display = path.display().to_string();
    eprintln!("[chat_log] Logging to: {path_display}");

    let writer = ChatLogMakeWriter { path };

    tracing_subscriber::fmt::layer()
        .json()
        .with_writer(writer)
        .with_target(true)          // показываем module path
        .with_current_span(false)   // спаны не нужны — используем поля
        .with_file(false)
        .with_line_number(false)
}
