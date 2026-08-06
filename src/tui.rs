// ---------------------------------------------------------------------------
// tui — полноэкранный TUI-фронт для ai-agent
//
// Стиль/палитра — из grok-build (xai-grok-pager-render, Apache-2.0):
//   groknight: BG #0a0a0a, BG_STORM #141414, BG_HIGHLIGHT #242424, акцент RGB(122,162,247)
// Раскладка: скроллбек сообщений / поле ввода / статус-бар (в духе xai-grok-pager).
// События агента приходят через broadcast FrontendEvent (общая шина с WebSocket-фронтом).
// Запуск: ai-agent --tui
// ---------------------------------------------------------------------------

use std::error::Error;
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
use crate::provider::FallbackProvider;
use crate::tool_routing::frontend::FrontendEvent;

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

/// Одна строка скроллбека: текст + цвет.
#[derive(Debug, Clone)]
struct UILine {
    text: String,
    color: Color,
}

impl UILine {
    fn new(text: impl Into<String>, color: Color) -> Self {
        Self { text: text.into(), color }
    }
}

/// Форматировать событие фронтенда в строку UI.
fn format_event(ev: &FrontendEvent) -> Option<UILine> {
    use palette::*;
    match ev {
        FrontendEvent::AgentMessage { content } => {
            let wrapped = textwrap::wrap(content, 110);
            Some(UILine::new(format!("  {}", wrapped.join("\n  ")), TEXT))
        }
        FrontendEvent::ToolExecuting { tool_name, arguments } => {
            let args = arguments.replace('\n', " ");
            let args = if args.chars().count() > 90 {
                format!("{}…", args.chars().take(90).collect::<String>())
            } else {
                args
            };
            Some(UILine::new(format!("  🔧 {tool_name} {args}"), MUTED))
        }
        FrontendEvent::ToolResult { tool_name, result } => {
            let r = result.replace('\n', " ").chars().take(80).collect::<String>();
            Some(UILine::new(format!("  ✅ {tool_name}: {r}"), OK))
        }
        FrontendEvent::SafetyReviewRequired { tool_name, reason } => {
            Some(UILine::new(format!("  ⚠️  SAFETY: {tool_name} — {reason}"), WARN))
        }
        FrontendEvent::ContextBranched { branch_name, source_branch } => {
            Some(UILine::new(format!("  🌿 ветка {source_branch} → {branch_name}"), ACCENT))
        }
        FrontendEvent::ModelInfo { model_name } => {
            Some(UILine::new(format!("  модель: {model_name}"), MUTED))
        }
        FrontendEvent::Ping => None,
    }
}

struct App {
    lines: Vec<UILine>,
    input: String,
    scroll: u16,
    status: String,
    running: bool,
    last_lines: usize,
}

impl App {
    fn new(model: &str, branch: &str, tool_count: usize) -> Self {
        let mut app = Self {
            lines: Vec::new(),
            input: String::new(),
            scroll: 0,
            status: format!("{model} · ветка {branch} · тулы {tool_count}"),
            running: false,
            last_lines: 0,
        };
        app.lines.push(UILine::new(
            "AI Agent TUI (стиль grok-build). Enter — отправить, /help — команды, Ctrl+C — выход.",
            palette::MUTED,
        ));
        app
    }

    fn push(&mut self, line: UILine) {
        self.lines.push(line);
    }

    fn push_user(&mut self, text: &str) {
        self.lines.push(UILine::new(format!("❯ {text}"), palette::ACCENT));
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

        // Скроллбек: показываем последние N строк, скролл — вверх от конца.
        let max_lines = (chunks[0].height as usize).saturating_sub(2);
        let start = self.lines.len().saturating_sub(max_lines).saturating_sub(self.scroll as usize);
        let items: Vec<ListItem> = self.lines[start..].iter().map(|l| {
            ListItem::new(Line::from(vec![Span::styled(l.text.clone(), Style::default().fg(l.color))]))
        }).collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" ai-agent ")
                .border_style(Style::default().fg(palette::ACCENT)))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD));
        f.render_widget(list, chunks[0]);

        // Поле ввода.
        let input_widget = Paragraph::new(self.input.as_str())
            .style(Style::default().fg(palette::TEXT))
            .block(Block::default().borders(Borders::ALL).title(" ввод ")
                .border_style(Style::default().fg(if self.running { palette::MUTED } else { palette::ACCENT })));
        f.render_widget(input_widget, chunks[1]);
        f.set_cursor_position((
            chunks[1].x + 1 + (self.input.chars().count() as u16).min(chunks[1].width.saturating_sub(2)),
            chunks[1].y + 1,
        ));

        // Статус-бар.
        let status = Paragraph::new(Line::from(vec![
            Span::styled("◆", Style::default().fg(if self.running { palette::OK } else { palette::ACCENT })),
            Span::styled(format!("  {}", self.status), Style::default().fg(palette::MUTED)),
        ]))
        .alignment(Alignment::Left)
        .style(Style::default().bg(palette::BG_STORM));
        f.render_widget(status, chunks[2]);

        self.last_lines = self.lines.len();
    }
}

/// Запустить TUI-цикл (взамен stdin-цикла в main.rs).
/// `agent` — в Arc<Mutex<>>, чтобы агент-таск мог работать параллельно с отрисовкой.
pub async fn run_tui(
    agent: Arc<tokio::sync::Mutex<Agent<FallbackProvider>>>,
    model: String,
    frontend_tx: broadcast::Sender<FrontendEvent>,
) -> Result<(), Box<dyn Error>> {
    // Terminal setup.
    crossterm::terminal::enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Текущее состояние для статус-бара.
    let (branch_name, tool_count) = {
        let a = agent.lock().await;
        (a.context.current_branch().name.clone(), a.router.len())
    };

    let mut app = App::new(&model, &branch_name, tool_count);
    let mut events = event::EventStream::new();
    let (result_tx, mut result_rx) = mpsc::channel::<(String, String)>(16); // (line, error)
    let mut frontend_rx = frontend_tx.subscribe();
    let mut history: Vec<String> = Vec::new();
    let mut hist_idx: usize = 0;

    let res = loop {
        // Тик для отрисовки (30 fps) + события.
        let tick = tokio::time::sleep(Duration::from_millis(33));
        tokio::pin!(tick);

        tokio::select! {
            // Клавиатура.
            ev = events.next() => {
                match ev {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        match key.code {
                            KeyCode::Char(c) if key.modifiers.contains(event::KeyModifiers::CONTROL) && c == 'c' => break,
                            KeyCode::Char(c) => { app.input.push(c); }
                            KeyCode::Backspace => { app.input.pop(); }
                            KeyCode::Enter => {
                                let text = app.input.trim().to_string();
                                if text.is_empty() { continue; }
                                history.push(text.clone());
                                hist_idx = history.len();
                                app.push_user(&text);
                                app.input.clear();
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
                                    let err = out.err().map(|e| e.to_string()).unwrap_or_default();
                                    let _ = rt.send((String::new(), err)).await;
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
                    Some(Err(_)) => break,
                    None => break,
                }
            }
            // События агента (общая шина с WebSocket).
            ev = frontend_rx.recv() => {
                if let Ok(ev) = ev {
                    if let Some(line) = format_event(&ev) {
                        app.push(line);
                    }
                }
            }
            // Результат генерации (спаун-таск).
            Some((_line, err)) = result_rx.recv() => {
                app.running = false;
                app.status = format!("{model} · ветка {branch_name}");
                app.push(UILine::new(if err.is_empty() { "  ✓ готово".into() } else { format!("  ✗ {err}") },
                    if err.is_empty() { palette::OK } else { palette::ERR }));
            }
            _ = &mut tick => {
                terminal.draw(|f| app.render(f))?;
            }
        }
    };

    // Cleanup.
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen)?;
    let _ = res;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_agent_message() {
        let ev = FrontendEvent::AgentMessage { content: "привет".into() };
        let l = format_event(&ev).expect("message formats");
        assert!(l.text.contains("привет"));
    }

    #[test]
    fn formats_tool_event() {
        let ev = FrontendEvent::ToolExecuting {
            tool_name: "shell".into(),
            arguments: "echo hi\nwith newline".into(),
        };
        let l = format_event(&ev).expect("tool formats");
        assert!(l.text.contains("shell"));
        assert!(!l.text.contains('\n')); // аргументы схлопнуты в одну строку
    }

    #[test]
    fn ping_skipped() {
        assert!(format_event(&FrontendEvent::Ping).is_none());
    }
}
