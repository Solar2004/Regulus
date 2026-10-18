//! Drawing the Operate view in the terminal: profiles and executions on the left, the
//! nodes of the selected execution, and what the selected node says on the right.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use regulus_proto::ExecutionState;

use super::app::{App, Focus, NodeKind, NodeState, Tone, Turn};
use super::core::{Connection, Message, SENDER};
use super::palette::{palette, Palette};

const SIDEBAR_WIDTH: u16 = 30;
const NODES_WIDTH: u16 = 34;

pub fn draw(frame: &mut Frame, app: &App) {
    let p = palette();
    let area = frame.area();
    frame.render_widget(
        Block::new().style(Style::default().bg(p.canvas).fg(p.text)),
        area,
    );
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(frame, app, header);
    let [side, nodes, detail] = Layout::horizontal([
        Constraint::Length(SIDEBAR_WIDTH),
        Constraint::Length(NODES_WIDTH),
        Constraint::Min(20),
    ])
    .areas(body);
    let [profiles, executions] =
        Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(side);
    draw_profiles(frame, app, profiles);
    draw_executions(frame, app, executions);
    draw_nodes(frame, app, nodes);
    draw_detail(frame, app, detail);
    draw_footer(frame, app, footer);
}

fn pane<'a>(title: &'a str, focused: bool, p: &Palette) -> Block<'a> {
    let border = if focused { p.focus } else { p.border };
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(if focused { p.focus } else { p.text_secondary })
                .add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(p.surface))
}

/// A state is always a symbol and a word, never color alone (docs/08-design-system.md).
fn state_span(symbol: &str, label: String, color: ratatui::style::Color) -> Span<'static> {
    Span::styled(format!("{symbol} {label}"), Style::default().fg(color))
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let status = match &app.connection {
        Connection::Connecting => state_span("…", "connecting".into(), p.warning),
        Connection::Starting => state_span("…", "starting the core".into(), p.warning),
        Connection::Connected { core, proto } => {
            state_span("✓", format!("{core} · proto {proto}"), p.success)
        }
        Connection::Failed(_) => state_span("✗", "no core".into(), p.danger),
    };
    let brand = Span::styled(
        " ★ REGULUS ",
        Style::default()
            .fg(p.on_accent)
            .bg(p.accent)
            .add_modifier(Modifier::BOLD),
    );
    let width = area.width as usize;
    let used = brand.width() + status.width() + 1;
    let line = Line::from(vec![
        brand,
        Span::raw(" ".repeat(width.saturating_sub(used))),
        status,
        Span::raw(" "),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(p.sidebar)),
        area,
    );
}

fn list<'a>(items: Vec<ListItem<'a>>, block: Block<'a>, p: &Palette) -> List<'a> {
    List::new(items)
        .block(block)
        .highlight_style(Style::default().bg(p.selected).add_modifier(Modifier::BOLD))
        .highlight_symbol("▸ ")
}

fn draw_profiles(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let focused = app.focus == Focus::Profiles;
    let items: Vec<ListItem> = app
        .profiles
        .iter()
        .map(|profile| ListItem::new(profile.name.clone()))
        .collect();
    let mut state = ListState::default().with_selected((!items.is_empty()).then_some(app.profile));
    if items.is_empty() {
        let text = Paragraph::new(Line::styled(
            "no profiles",
            Style::default().fg(p.text_muted),
        ))
        .block(pane("Profiles", focused, p));
        frame.render_widget(text, area);
        return;
    }
    frame.render_stateful_widget(
        list(items, pane("Profiles", focused, p), p),
        area,
        &mut state,
    );
}

fn execution_state(state: &ExecutionState, p: &Palette) -> Span<'static> {
    match state {
        ExecutionState::Starting => state_span("…", "starting".into(), p.warning),
        ExecutionState::Running => state_span("✓", "running".into(), p.success),
        ExecutionState::Stopped => state_span("○", "stopped".into(), p.text_muted),
        ExecutionState::Failed { .. } => state_span("✗", "failed".into(), p.danger),
    }
}

fn draw_executions(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let focused = app.focus == Focus::Executions;
    if app.executions.is_empty() {
        let text = Paragraph::new(vec![
            Line::styled("nothing running", Style::default().fg(p.text_muted)),
            Line::styled(
                "Enter on a profile runs it",
                Style::default().fg(p.text_muted),
            ),
        ])
        .block(pane("Executions", focused, p));
        frame.render_widget(text, area);
        return;
    }
    let items: Vec<ListItem> = app
        .executions
        .iter()
        .map(|execution| {
            ListItem::new(vec![
                Line::from(vec![
                    Span::raw(format!("{} ", execution.id)),
                    execution_state(&execution.state, p),
                ]),
                Line::styled(
                    format!("  {} / {}", execution.profile_id, execution.workflow_id),
                    Style::default().fg(p.text_muted),
                ),
            ])
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.execution));
    frame.render_stateful_widget(
        list(items, pane("Executions", focused, p), p),
        area,
        &mut state,
    );
}

fn node_state(state: &NodeState, p: &Palette) -> Span<'static> {
    match state {
        NodeState::Running => state_span("✓", "running".into(), p.success),
        NodeState::Resource => state_span("·", "resource".into(), p.text_muted),
        NodeState::NotInstalled => state_span("□", "not installed".into(), p.text_muted),
        NodeState::Disabled(missing) => state_span(
            "✗",
            format!("disabled: needs {}", missing.join(", ")),
            p.danger,
        ),
        NodeState::Broken(_) => state_span("▲", "broken".into(), p.danger),
        NodeState::Stopped => state_span("○", "stopped".into(), p.text_muted),
    }
}

fn draw_nodes(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let focused = app.focus == Focus::Nodes;
    let rows = app.nodes();
    if rows.is_empty() {
        let text = Paragraph::new(Line::styled(
            "select an execution",
            Style::default().fg(p.text_muted),
        ))
        .block(pane("Nodes", focused, p));
        frame.render_widget(text, area);
        return;
    }
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let marker = match row.kind {
                NodeKind::Agent => "@",
                NodeKind::Channel => "#",
                NodeKind::Other => " ",
            };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled(
                        format!("{marker}{} ", row.name),
                        Style::default().fg(p.text),
                    ),
                    Span::styled(row.node_type.clone(), Style::default().fg(p.steel)),
                ]),
                Line::from(vec![Span::raw("  "), node_state(&row.state, p)]),
            ])
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(app.node));
    frame.render_stateful_widget(list(items, pane("Nodes", focused, p), p), area, &mut state);
}

/// Splits `text` into lines no wider than `width` characters, between words when it can.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for line in text.lines() {
        let mut current = String::new();
        let mut used = 0;
        for word in line.split(' ') {
            let mut word: Vec<char> = word.chars().collect();
            let gap = usize::from(used > 0);
            if used + gap + word.len() > width && used > 0 {
                out.push(std::mem::take(&mut current));
                used = 0;
            } else if gap == 1 {
                current.push(' ');
                used += 1;
            }
            // A word longer than the line is cut.
            while word.len() > width {
                let rest = word.split_off(width);
                out.push(word.into_iter().collect());
                word = rest;
            }
            used += word.len();
            current.extend(word);
        }
        out.push(current);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// `who › text`, wrapped, the name in `color`.
fn said(who: &str, text: &str, color: ratatui::style::Color, width: usize) -> Vec<Line<'static>> {
    let p = palette();
    let prefix = format!("{who} › ");
    let indent = " ".repeat(prefix.chars().count().min(width / 2));
    let room = width.saturating_sub(indent.len());
    wrap(text, room)
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let head = if index == 0 {
                Span::styled(
                    prefix.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw(indent.clone())
            };
            Line::from(vec![head, Span::styled(chunk, Style::default().fg(p.text))])
        })
        .collect()
}

fn message_lines(message: &Message, width: usize) -> Vec<Line<'static>> {
    let p = palette();
    let reply = message
        .reply_to
        .map(|id| format!(" re #{id}"))
        .unwrap_or_default();
    let who = format!(
        "#{} {} → {}{reply}",
        message.id,
        message.from,
        message.to.join(", ")
    );
    match &message.error {
        Some(error) => said(&who, &format!("(not delivered: {error})"), p.danger, width),
        None => {
            let color = if message.from == SENDER {
                p.focus
            } else {
                p.info
            };
            said(&who, &message.text, color, width)
        }
    }
}

/// The last lines that fit in `height`.
fn tail(mut lines: Vec<Line<'static>>, height: usize) -> Vec<Line<'static>> {
    let skip = lines.len().saturating_sub(height);
    lines.drain(..skip);
    lines
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let target = app.talk_target();
    let typing = app.focus == Focus::Input;
    let (body, input) = if target.is_some() {
        let [body, input] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).areas(area);
        (body, Some(input))
    } else {
        (area, None)
    };
    let width = body.width.saturating_sub(2) as usize;
    let height = body.height.saturating_sub(2) as usize;
    let muted = Style::default().fg(p.text_muted);

    let (title, lines) = match (app.selected_node(), &target) {
        (Some(node), Some((key, NodeKind::Agent))) => {
            let mut lines = Vec::new();
            for turn in app.turns.get(key).into_iter().flatten() {
                lines.extend(match turn {
                    Turn::Prompt(text) => said("you", text, p.focus, width),
                    Turn::Answer(text) => said(&node.name, text, p.info, width),
                    Turn::Error(error) => said("error", error, p.danger, width),
                });
                lines.push(Line::raw(""));
            }
            if app.thinking.contains(key) {
                lines.push(Line::styled(
                    format!("{} is working…", node.name),
                    Style::default().fg(p.warning),
                ));
            }
            if lines.is_empty() {
                lines.push(Line::styled("press Enter and write a prompt", muted));
            }
            (format!("@{} · agent", node.name), lines)
        }
        (Some(node), Some((key, NodeKind::Channel))) => {
            let mut lines = Vec::new();
            match app.channels.get(key) {
                None => lines.push(Line::styled("loading…", muted)),
                Some(Err(error)) => lines.extend(
                    wrap(error, width)
                        .into_iter()
                        .map(|l| Line::styled(l, Style::default().fg(p.danger))),
                ),
                Some(Ok(channel)) => {
                    let who: Vec<Span> = channel
                        .participants
                        .iter()
                        .flat_map(|participant| {
                            let color = match participant.state.as_str() {
                                "idle" => p.success,
                                "busy" => p.warning,
                                "gone" => p.danger,
                                _ => p.text_muted,
                            };
                            let state = if participant.state.is_empty() {
                                "unknown"
                            } else {
                                &participant.state
                            };
                            let queued = if participant.queued > 0 {
                                format!(" ({} queued)", participant.queued)
                            } else {
                                String::new()
                            };
                            [
                                Span::styled(
                                    format!("@{} ", participant.name),
                                    Style::default().fg(p.text),
                                ),
                                Span::styled(
                                    format!("{state}{queued}  "),
                                    Style::default().fg(color),
                                ),
                            ]
                        })
                        .collect();
                    lines.push(Line::from(who));
                    lines.push(Line::styled(
                        "─".repeat(width),
                        Style::default().fg(p.border),
                    ));
                    if channel.messages.is_empty() {
                        lines.push(Line::styled(
                            "no messages yet; @name addresses someone",
                            muted,
                        ));
                    }
                    let head = lines.len();
                    let mut log = Vec::new();
                    for message in &channel.messages {
                        log.extend(message_lines(message, width));
                    }
                    lines.extend(tail(log, height.saturating_sub(head)));
                }
            }
            (format!("#{} · channel", node.name), lines)
        }
        (Some(node), _) => {
            let mut lines = vec![
                Line::from(vec![
                    Span::styled("type    ", muted),
                    Span::raw(node.node_type.clone()),
                ]),
                Line::from(vec![
                    Span::styled("state   ", muted),
                    node_state(&node.state, p),
                ]),
            ];
            if let NodeState::Broken(reason) = &node.state {
                lines.push(Line::styled(reason.clone(), Style::default().fg(p.danger)));
            }
            let hint = match (&node.state, node.kind) {
                (_, NodeKind::Other) => None,
                (NodeState::Disabled(missing), _) => Some(format!(
                    "it takes input once something is wired into {}",
                    missing.join(", ")
                )),
                _ => Some("it takes input while its execution runs".to_string()),
            };
            if let Some(hint) = hint {
                lines.push(Line::raw(""));
                lines.extend(
                    wrap(&hint, width)
                        .into_iter()
                        .map(|l| Line::styled(l, muted)),
                );
            }
            (node.name.clone(), lines)
        }
        (None, _) => {
            let mut lines = Vec::new();
            if let Connection::Failed(reason) = &app.connection {
                lines.push(Line::styled(
                    "No core answered.",
                    Style::default().fg(p.danger),
                ));
                lines.extend(
                    wrap(reason, width)
                        .into_iter()
                        .map(|l| Line::styled(l, muted)),
                );
            } else if let Some(ExecutionState::Failed { reason }) =
                app.selected_execution().map(|e| &e.state)
            {
                lines.push(Line::styled(
                    "The execution failed:",
                    Style::default().fg(p.danger),
                ));
                lines.extend(
                    wrap(reason, width)
                        .into_iter()
                        .map(|l| Line::styled(l, muted)),
                );
            } else {
                lines.push(Line::styled(
                    "Pick a profile and press Enter to run it,",
                    muted,
                ));
                lines.push(Line::styled("then choose an agent or a channel.", muted));
            }
            ("Regulus".to_string(), lines)
        }
    };
    let detail_focus = typing;
    frame.render_widget(
        Paragraph::new(tail(lines, height)).block(pane(&title, detail_focus, p)),
        body,
    );

    if let (Some(input), Some((_, kind))) = (input, target) {
        let hint = match kind {
            NodeKind::Agent => "prompt",
            _ => "message (@name to address)",
        };
        let shown = if app.input.is_empty() && !typing {
            Line::styled(format!("Enter to write a {hint}"), muted)
        } else {
            let room = input.width.saturating_sub(4) as usize;
            let chars: Vec<char> = app.input.chars().collect();
            let visible: String = chars[chars.len().saturating_sub(room)..].iter().collect();
            Line::from(vec![
                Span::styled("› ", Style::default().fg(p.focus)),
                Span::raw(visible),
            ])
        };
        frame.render_widget(Paragraph::new(shown).block(pane(hint, typing, p)), input);
        if typing {
            let room = input.width.saturating_sub(4) as usize;
            let typed = app.input.chars().count().min(room) as u16;
            frame.set_cursor_position((input.x + 3 + typed, input.y + 1));
        }
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let line = match &app.notice {
        Some((Tone::Error, text)) => {
            Line::styled(format!(" ✗ {text}"), Style::default().fg(p.danger))
        }
        Some((Tone::Info, text)) => Line::styled(format!(" {text}"), Style::default().fg(p.info)),
        None => {
            let keys = match app.focus {
                Focus::Input => "Enter send · Esc back · Ctrl-C quit",
                Focus::Profiles => "Enter run · ↑↓ move · Tab next pane · r refresh · q quit",
                Focus::Executions => "s stop · Enter nodes · ↑↓ move · Tab next pane · q quit",
                Focus::Nodes => "Enter write · ↑↓ move · Tab next pane · q quit",
            };
            Line::styled(format!(" {keys}"), Style::default().fg(p.text_muted))
        }
    };
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(p.sidebar)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn screen(app: &App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(120)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn without_a_core_it_says_why() {
        let app = App {
            connection: Connection::Failed("start one with: regulus serve".into()),
            ..App::default()
        };
        let shown = screen(&app);
        assert!(shown.contains("no core"), "{shown}");
        assert!(shown.contains("start one with: regulus serve"), "{shown}");
    }

    #[test]
    fn long_text_wraps_to_the_width() {
        assert_eq!(wrap("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap("one two three", 8), vec!["one two", "three"]);
        assert_eq!(wrap("", 4), vec![""]);
        assert_eq!(wrap("a\n\nb", 4), vec!["a", "", "b"]);
    }
}
