// ---------------------------------------------------------------------------
// tui — полноэкранный TUI-фронт для ai-agent
//
// Стиль/палитра — из grok-build (xai-grok-pager-render, Apache-2.0):
//   groknight: BG #0a0a0a, BG_STORM #141414, BG_HIGHLIGHT #242424, акцент RGB(122,162,247)
// Раскладка: скроллбек сообщений / поле ввода / статус-бар (в духе xai-grok-pager).
// Докрутка:
//   - лёгкий markdown-хайлайтер (заголовки, **bold**, *italic*, `code`, ```fences```)
//   - slash-команды: /help /tools /branch [name] /clear /plan <file.luck> /quit
//   - ветка контекста в статус-баре и переключение
// Запуск: ai-agent --tui
// ---------------------------------------------------------------------------

use std::error::Error;
use std::fs;
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use futures_util::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::{Frame, Terminal};
use tokio::sync::{broadcast, mpsc};

use crate::agent::Agent;
use crate::luck_compile::compile as compile_luck;
use crate::luck_scheduler::{PlanEvent, PlanExecutor, PlanOutcome, PlanRuntime};
use crate::provider::{FallbackProvider, ModelProvider};
use crate::tool_routing::frontend::FrontendEvent;
use crate::types::{Message, Role};
use serde_json::Value;

// Палитра groknight (из grok-build, Apache-2.0).
mod palette {
    use ratatui::style::Color;
    pub const BG_STORM: Color = Color::Rgb(20, 20, 20);
    pub const ACCENT: Color = Color::Rgb(122, 162, 247);
    pub const TEXT: Color = Color::Rgb(220, 220, 220);
    pub const MUTED: Color = Color::Rgb(120, 120, 120);
    pub const OK: Color = Color::Rgb(120, 220, 120);
    pub const WARN: Color = Color::Rgb(240, 200, 90);
    pub const ERR: Color = Color::Rgb(240, 110, 110);
}

/// Строка скроллбека: набор Span'ов (поддерживает markdown-разметку).
#[derive(Debug, Clone)]
struct UILine {
    spans: Vec<Span<'static>>,
}

impl UILine {
    fn plain(text: impl Into<String>, color: Color) -> Self {
        Self { spans: vec![Span::styled(text.into(), Style::default().fg(color))] }
    }

    /// Markdown-строка с подсветкой.
    fn markdown(text: &str) -> Self {
        Self { spans: markdown_spans(text, palette::TEXT) }
    }

    #[cfg(test)]
    fn text(&self) -> String {
        self.spans.iter().map(|s| s.content.as_ref().to_string()).collect()
    }
}

/// Лёгкий markdown-хайлайтер (v1: заголовки, **bold**, *italic*, `code`, ```fences```).
fn markdown_spans(text: &str, base: Color) -> Vec<Span<'static>> {
    use palette::*;
    let trimmed = text.trim_start();
    // ```fence``` — весь блок приглушённым.
    if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
        return vec![Span::styled(text.to_string(), Style::default().fg(MUTED))];
    }
    // Заголовок.
    let header_level = trimmed.chars().take_while(|&c| c == '#').count();
    if header_level > 0 && trimmed.len() > header_level && trimmed.as_bytes()[header_level] == b' ' {
        let content = trimmed[header_level..].trim();
        let style = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
        return vec![Span::styled(format!("{} {}", "#".repeat(header_level), content), style)];
    }

    // Разбиваем по обратным кавычкам: нечётные сегменты — inline-код.
    let mut spans = Vec::new();
    for (i, seg) in text.split('`').enumerate() {
        if i % 2 == 1 {
            // inline-код.
            spans.push(Span::styled(
                seg.to_string(),
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ));
        } else {
            // обычный текст: выделяем **bold** и *italic* (один уровень).
            let mut plain = String::new();
            let mut j = 0usize;
            while j < seg.len() {
                let ch = seg[j..].chars().next().unwrap();
                if seg[j..].starts_with("**") {
                    if !plain.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut plain), Style::default().fg(base)));
                    }
                    let end = seg[j + 2..].find("**");
                    match end {
                        Some(e) => {
                            let inner = &seg[j + 2..j + 2 + e];
                            spans.push(Span::styled(
                                inner.to_string(),
                                Style::default().fg(base).add_modifier(Modifier::BOLD),
                            ));
                            j += 2 + e + 2;
                        }
                        None => {
                            plain.push_str(&seg[j..]);
                            j = seg.len();
                        }
                    }
                } else if ch == '*' {
                    if !plain.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut plain), Style::default().fg(base)));
                    }
                    let end = seg[j + 1..].find('*');
                    match end {
                        Some(e) => {
                            let inner = &seg[j + 1..j + 1 + e];
                            spans.push(Span::styled(
                                inner.to_string(),
                                Style::default().fg(base).add_modifier(Modifier::ITALIC),
                            ));
                            j += 1 + e + 1;
                        }
                        None => {
                            plain.push('*');
                            j += 1;
                        }
                    }
                } else {
                    plain.push(ch);
                    j += ch.len_utf8();
                }
            }
            if !plain.is_empty() {
                spans.push(Span::styled(plain, Style::default().fg(base)));
            }
        }
    }
    if spans.is_empty() {
        spans.push(Span::styled(text.to_string(), Style::default().fg(base)));
    }
    spans
}

/// Форматировать событие фронтенда в строку UI.
fn format_event(ev: &FrontendEvent) -> Option<UILine> {
    use palette::*;
    match ev {
        FrontendEvent::AgentMessage { content } => {
            // Сообщение агента — с markdown-подсветкой, по строкам.
            let mut lines = Vec::new();
            for l in content.lines() {
                lines.push(UILine::markdown(l));
            }
            // Складываем в одну строку через перенос (ListItem поддерживает multi-line Line? нет —
            // поэтому каждая строка становится отдельной UILine, но нам нужен один элемент).
            // Упрощение: рендерим как одну UILine с переносами внутри одного Span-набора.
            if lines.len() == 1 {
                return lines.into_iter().next();
            }
            let mut spans = Vec::new();
            for (i, l) in lines.iter().enumerate() {
                if i > 0 {
                    spans.push(Span::raw("\n"));
                }
                spans.extend(l.spans.iter().cloned());
            }
            Some(UILine { spans })
        }
        FrontendEvent::ToolExecuting { tool_name, arguments } => {
            let args = arguments.replace('\n', " ");
            let args = if args.chars().count() > 90 {
                format!("{}…", args.chars().take(90).collect::<String>())
            } else {
                args
            };
            Some(UILine::plain(format!("  🔧 {tool_name} {args}"), MUTED))
        }
        FrontendEvent::ToolResult { tool_name, result } => {
            let r = result.replace('\n', " ").chars().take(80).collect::<String>();
            Some(UILine::plain(format!("  ✅ {tool_name}: {r}"), OK))
        }
        FrontendEvent::SafetyReviewRequired { tool_name, reason } => {
            Some(UILine::plain(format!("  ⚠️  SAFETY: {tool_name} — {reason}"), WARN))
        }
        FrontendEvent::ContextBranched { branch_name, source_branch } => {
            Some(UILine::plain(format!("  🌿 ветка {source_branch} → {branch_name}"), ACCENT))
        }
        FrontendEvent::ModelInfo { model_name } => {
            Some(UILine::plain(format!("  модель: {model_name}"), MUTED))
        }
        FrontendEvent::PlanProgress { node, status } => {
            let (icon, color) = match status.as_str() {
                "start" => ("▶", ACCENT),
                "ok" => ("✓", OK),
                "fail" => ("✗", ERR),
                "reject" => ("⛔", ERR),
                _ => ("•", MUTED),
            };
            Some(UILine::plain(format!("  {icon} {node}"), color))
        }
        FrontendEvent::Ping => None,
    }
}

/// Рантайм плана поверх агента: generate → provider, call_tool → router.
pub struct TuiRuntime {
    agent: Arc<tokio::sync::Mutex<Agent<FallbackProvider>>>,
    model: String,
}

impl TuiRuntime {
    pub fn new(agent: Arc<tokio::sync::Mutex<Agent<FallbackProvider>>>, model: String) -> Self {
        Self { agent, model }
    }
}

#[async_trait::async_trait]
impl PlanRuntime for TuiRuntime {
    async fn generate(&self, system: Option<&str>, user: &str) -> Result<String, String> {
        let mut messages = Vec::new();
        if let Some(sys) = system {
            messages.push(Message::new(Role::System, sys));
        }
        messages.push(Message::new(Role::User, user));
        let a = self.agent.lock().await;
        let stream = a
            .provider
            .stream_chat(&self.model, messages, None)
            .await
            .map_err(|e| e.to_string())?;
        let mut out = String::new();
        let mut pin = stream;
        while let Some(chunk) = pin.next().await {
            if let Ok(c) = chunk {
                if let Some(d) = c.delta_content {
                    out.push_str(&d);
                }
            }
        }
        Ok(out)
    }

    async fn call_tool(&self, name: &str, args: &Value) -> Result<String, String> {
        let args_str = serde_json::to_string(args).map_err(|e| e.to_string())?;
        let a = self.agent.lock().await;
        let tool = a
            .router
            .get(name)
            .ok_or_else(|| format!("tool not found: {name}"))?;
        tool.execute(&args_str).await
    }
}

/// Получить событие плана (или вечно ждать, если план не запущен).
async fn recv_plan(rx: &mut Option<mpsc::Receiver<PlanEvent>>) -> Option<Option<PlanEvent>> {
    match rx {
        Some(r) => Some(r.recv().await),
        None => std::future::pending().await,
    }
}

struct App {
    lines: Vec<UILine>,
    input: String,
    scroll: u16,
    status: String,
    branch: String,
    running: bool,
}

impl App {
    fn new(model: &str, branch: &str, tool_count: usize) -> Self {
        let mut app = Self {
            lines: Vec::new(),
            input: String::new(),
            scroll: 0,
            status: format!("{model} · ветка {branch} · тулы {tool_count}"),
            branch: branch.to_string(),
            running: false,
        };
        app.push(UILine::plain(
            "AI Agent TUI (стиль grok-build). /help — команды, Enter — отправить, Ctrl+C — выход.",
            palette::MUTED,
        ));
        app
    }

    fn push(&mut self, line: UILine) {
        self.lines.push(line);
    }

    fn push_user(&mut self, text: &str) {
        self.lines.push(UILine::plain(format!("❯ {text}"), palette::ACCENT));
    }

    fn render(&mut self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(3),
                Constraint::Length(3),
                Constraint::Length(1),
            ])
            .split(f.area());

        let max_lines = (chunks[0].height as usize).saturating_sub(2);
        let start = self.lines.len().saturating_sub(max_lines).saturating_sub(self.scroll as usize);
        let items: Vec<ListItem> = self.lines[start..]
            .iter()
            .map(|l| ListItem::new(Line::from(l.spans.clone())))
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" ai-agent ")
                .border_style(Style::default().fg(palette::ACCENT)))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD));
        f.render_widget(list, chunks[0]);

        let input_widget = Paragraph::new(self.input.as_str())
            .style(Style::default().fg(palette::TEXT))
            .block(Block::default().borders(Borders::ALL).title(" ввод ")
                .border_style(Style::default().fg(if self.running { palette::MUTED } else { palette::ACCENT })));
        f.render_widget(input_widget, chunks[1]);
        f.set_cursor_position((
            chunks[1].x + 1 + (self.input.chars().count() as u16).min(chunks[1].width.saturating_sub(2)),
            chunks[1].y + 1,
        ));

        let status = Paragraph::new(Line::from(vec![
            Span::styled("◆", Style::default().fg(if self.running { palette::OK } else { palette::ACCENT })),
            Span::styled(format!("  {}", self.status), Style::default().fg(palette::MUTED)),
        ]))
        .alignment(Alignment::Left)
        .style(Style::default().bg(palette::BG_STORM));
        f.render_widget(status, chunks[2]);
    }
}

/// Запустить TUI-цикл (взамен stdin-цикла в main.rs).
pub async fn run_tui(
    agent: Arc<tokio::sync::Mutex<Agent<FallbackProvider>>>,
    model: String,
    frontend_tx: broadcast::Sender<FrontendEvent>,
) -> Result<(), Box<dyn Error>> {
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let (branch_name, tool_count, tools) = {
        let a = agent.lock().await;
        (
            a.context.current_branch().name.clone(),
            a.router.len(),
            a.router.tool_names().into_iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    };

    let mut app = App::new(&model, &branch_name, tool_count);
    let mut events = event::EventStream::new();
    let (result_tx, mut result_rx) = mpsc::channel::<(String, String)>(16);
    let mut frontend_rx = frontend_tx.subscribe();
    let mut history: Vec<String> = Vec::new();
    let mut hist_idx: usize = 0;
    let mut quit = false;
    let mut plan_rx: Option<mpsc::Receiver<PlanEvent>> = None;

    while !quit {
        let tick = tokio::time::sleep(Duration::from_millis(33));
        tokio::pin!(tick);

        tokio::select! {
            ev = events.next() => {
                match ev {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        match key.code {
                            KeyCode::Char(c) if key.modifiers.contains(event::KeyModifiers::CONTROL) && c == 'c' => quit = true,
                            KeyCode::Char(c) => { app.input.push(c); }
                            KeyCode::Backspace => { app.input.pop(); }
                            KeyCode::Enter => {
                                let text = app.input.trim().to_string();
                                if text.is_empty() { continue; }
                                history.push(text.clone());
                                hist_idx = history.len();
                                app.push_user(&text);
                                app.input.clear();

                                if text.starts_with('/') {
                                    // ---- slash-команды -------------------------------
                                    let mut cmd_parts = text.splitn(2, char::is_whitespace);
                                    let cmd = cmd_parts.next().unwrap_or("");
                                    let arg = cmd_parts.next().unwrap_or("").trim();
                                    match cmd {
                                        "/help" => {
                                            app.push(UILine::plain("  Команды:", palette::ACCENT));
                                            app.push(UILine::plain("  <текст> — обычный запрос к модели (Enter отправляет)", palette::TEXT));
                                            app.push(UILine::plain("  /plan <file.luck> — скомпилировать и ИСПОЛНИТЬ план", palette::TEXT));
                                            app.push(UILine::plain("      пример: /plan examples/demo.luck", palette::MUTED));
                                            app.push(UILine::plain("  /tools — список зарегистрированных тулов", palette::TEXT));
                                            app.push(UILine::plain("  /branch — список веток контекста", palette::TEXT));
                                            app.push(UILine::plain("  /branch <name> — переключиться на ветку", palette::TEXT));
                                            app.push(UILine::plain("  /clear — очистить экран", palette::TEXT));
                                            app.push(UILine::plain("  /quit — выход (или Ctrl+C)", palette::TEXT));
                                            app.push(UILine::plain("  ↑/↓ — история, PageUp/PageDown — скролл", palette::MUTED));
                                        }
                                        "/tools" => {
                                            for t in &tools { app.push(UILine::plain(format!("  • {t}"), palette::TEXT)); }
                                            if tools.is_empty() { app.push(UILine::plain("  (тулов нет)", palette::MUTED)); }
                                        }
                                        "/branch" => {
                                            let mut a = agent.lock().await;
                                            let list = a.context.list();
                                            for b in list {
                                                let cur = if b.name == app.branch { " ◀" } else { "" };
                                                app.push(UILine::plain(format!("  • {} ({} сообщ.){cur}", b.name, a.context.current_messages().len()), palette::TEXT));
                                            }
                                            if arg.is_empty() {
                                                app.push(UILine::plain("  /branch <name> — переключиться", palette::MUTED));
                                            } else {
                                                match a.context.switch_by_name(arg) {
                                                    Ok(()) => {
                                                        app.branch = a.context.current_branch().name.clone();
                                                        app.status = format!("{model} · ветка {}", app.branch);
                                                        app.push(UILine::plain(format!("  🌿 переключено на «{arg}»"), palette::OK));
                                                    }
                                                    Err(e) => app.push(UILine::plain(format!("  ✗ {e}"), palette::ERR)),
                                                }
                                            }
                                        }
                                        "/clear" => {
                                            app.lines.clear();
                                            app.scroll = 0;
                                        }
                                        "/plan" => {
                                            if arg.is_empty() {
                                                app.push(UILine::plain("  /plan <file.luck> — скомпилировать и исполнить план", palette::MUTED));
                                            } else {
                                                match fs::read_to_string(arg) {
                                                    Ok(src) => match compile_luck(&src) {
                                                        Ok(plan) => {
                                                            app.push(UILine::plain(format!("  ✅ план: {} узлов, {} рёбер", plan.nodes.len(), plan.edges.len()), palette::OK));
                                                            for n in &plan.nodes {
                                                                app.push(UILine::plain(format!("    {}: {:?}{}", n.id, n.kind, n.into.as_ref().map(|k| format!(" → {k}")).unwrap_or_default()), palette::TEXT));
                                                            }
                                                            // Исполнение с прогрессом по узлам.
                                                            let (ptx, prx) = mpsc::channel::<PlanEvent>(32);
                                                            plan_rx = Some(prx);
                                                            let a2 = agent.clone();
                                                            let m2 = model.clone();
                                                            let rt = result_tx.clone();
                                                            app.running = true;
                                                            app.status = format!("план: исполняется…");
                                                            tokio::spawn(async move {
                                                                let runtime = TuiRuntime { agent: a2, model: m2 };
                                                                let mut ex = PlanExecutor::with_events(Arc::new(runtime), ptx);
                                                                let outcome = ex.run(&plan).await;
                                                                let err = match outcome {
                                                                    PlanOutcome::Completed { .. } => String::new(),
                                                                    PlanOutcome::Rejected { reason } => format!("план отклонён: {reason}"),
                                                                };
                                                                let _ = rt.send((String::new(), err)).await;
                                                            });
                                                        }
                                                        Err(e) => app.push(UILine::plain(format!("  ✗ компиляция: {e}"), palette::ERR)),
                                                    },
                                                    Err(e) => app.push(UILine::plain(format!("  ✗ файл: {e}"), palette::ERR)),
                                                }
                                            }
                                        }
                                        "/quit" => quit = true,
                                        other => {
                                            app.push(UILine::plain(format!("  ✗ неизвестная команда: {other}"), palette::ERR));
                                        }
                                    }
                                    continue;
                                }

                                // ---- обычный запрос к агенту -----------------------
                                app.running = true;
                                app.status = format!("{model} · работает…");
                                let a2 = agent.clone();
                                let m2 = model.clone();
                                let rt = result_tx.clone();
                                tokio::spawn(async move {
                                    let out = {
                                        let mut a = a2.lock().await;
                                        a.run(&m2).await
                                    };
                                    let (answer, err) = match out {
                                        Ok(a) => (a, String::new()),
                                        Err(e) => (String::new(), e.to_string()),
                                    };
                                    let _ = rt.send((answer, err)).await;
                                });
                            }
                            KeyCode::PageUp => { app.scroll = app.scroll.saturating_add(10); }
                            KeyCode::PageDown => { app.scroll = app.scroll.saturating_sub(10); }
                            KeyCode::Up => {
                                if hist_idx > 0 { hist_idx -= 1; }
                                if let Some(h) = history.get(hist_idx) { app.input = h.clone(); }
                            }
                            KeyCode::Down => {
                                if hist_idx < history.len() { hist_idx += 1; }
                                if let Some(h) = history.get(hist_idx) { app.input = h.clone(); } else { app.input.clear(); }
                            }
                            _ => {}
                        }
                    }
                    Some(Ok(Event::Key(_))) => {}
                    Some(Ok(_)) => {}
                    Some(Err(_)) => quit = true,
                    None => quit = true,
                }
            }
            ev = frontend_rx.recv() => {
                if let Ok(ev) = ev {
                    if let Some(line) = format_event(&ev) {
                        app.push(line);
                    }
                }
            }
            Some((line, err)) = result_rx.recv() => {
                app.running = false;
                app.status = format!("{model} · ветка {}", app.branch);
                if !line.is_empty() {
                    app.push(UILine::plain("  🤖", palette::ACCENT));
                    app.push(UILine::markdown(&line));
                }
                if !err.is_empty() {
                    app.push(UILine::plain(format!("  ✗ {err}"), palette::ERR));
                } else if line.is_empty() {
                    app.push(UILine::plain("  ✓ готово", palette::OK));
                }
            }
            pev = recv_plan(&mut plan_rx) => {
                match pev {
                    Some(Some(PlanEvent::NodeStart { id })) => {
                        app.push(UILine::plain(format!("  ▶ {id}"), palette::ACCENT));
                    }
                    Some(Some(PlanEvent::NodeDone { id, ok })) => {
                        app.push(UILine::plain(
                            format!("  {} {id}", if ok { "✓" } else { "✗" }),
                            if ok { palette::OK } else { palette::ERR },
                        ));
                    }
                    Some(Some(PlanEvent::Rejected { reason })) => {
                        app.push(UILine::plain(format!("  ⛔ {reason}"), palette::ERR));
                    }
                    Some(Some(PlanEvent::Completed)) => {
                        app.push(UILine::plain("  🏁 план завершён", palette::OK));
                    }
                    _ => {}
                }
            }
            _ = &mut tick => {
                terminal.draw(|f| app.render(f))?;
            }
        }
    }

    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_agent_message() {
        let ev = FrontendEvent::AgentMessage { content: "привет **мир**".into() };
        let l = format_event(&ev).expect("message formats");
        assert!(l.text().contains("привет мир"));
    }

    #[test]
    fn formats_tool_event() {
        let ev = FrontendEvent::ToolExecuting {
            tool_name: "shell".into(),
            arguments: "echo hi\nwith newline".into(),
        };
        let l = format_event(&ev).expect("tool formats");
        assert!(l.text().contains("shell"));
        assert!(!l.text().contains('\n'));
    }

    #[test]
    fn ping_skipped() {
        assert!(format_event(&FrontendEvent::Ping).is_none());
    }

    #[test]
    fn markdown_bold() {
        let spans = markdown_spans("a **bold** b", palette::TEXT);
        let has_bold = spans.iter().any(|s| s.style.add_modifier.contains(Modifier::BOLD));
        assert!(has_bold);
    }

    #[test]
    fn markdown_code() {
        let spans = markdown_spans("use `grep -r` now", palette::TEXT);
        assert!(spans.iter().any(|s| s.style.fg == Some(palette::ACCENT)));
    }

    #[test]
    fn markdown_header() {
        let spans = markdown_spans("## Заголовок", palette::TEXT);
        assert!(spans.iter().any(|s| s.style.add_modifier.contains(Modifier::BOLD)));
        let joined: String = spans.iter().map(|s| s.content.as_ref().to_string()).collect();
        assert!(joined.contains("Заголовок"));
    }

    #[test]
    fn markdown_fence_muted() {
        let spans = markdown_spans("```code block```", palette::TEXT);
        assert!(spans.iter().any(|s| s.style.fg == Some(palette::MUTED)));
    }
}
