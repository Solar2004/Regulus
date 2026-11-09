//! Drawing the Operate view in the terminal: profiles and executions on the left, the
//! nodes of the selected execution, and what the selected node says on the right.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use regulus_proto::ExecutionState;
use serde_json::Value;

use super::app::{compact, event_line, App, Focus, Mode, NodeKind, NodeState, Prompt, Tone};
use super::core::{describe, Connection, Message, SENDER};
use super::palette::{palette, Palette};
use super::talk::Choice as PickChoice;
use super::view_edit;
use crate::conversation::{reduce, BindingSource, Item, AGENT, SYSTEM, USER};

const SIDEBAR_WIDTH: u16 = 30;
const NODES_WIDTH: u16 = 34;
const DECISIONS_WIDTH: u16 = 44;
/// Share of the body the events and decisions take, in percent.
const LOWER_SHARE: u16 = 40;

pub fn draw(frame: &mut Frame, app: &App) {
    match &app.mode {
        Mode::Edit(editor) => view_edit::draw_edit(frame, app, editor),
        Mode::Templates(view) => view_edit::draw_templates(frame, app, view),
        Mode::Operate => draw_operate(frame, app),
    }
}

fn draw_operate(frame: &mut Frame, app: &App) {
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
    let [body, lower] = Layout::vertical([
        Constraint::Percentage(100 - LOWER_SHARE),
        Constraint::Percentage(LOWER_SHARE),
    ])
    .areas(body);
    let [events, decisions] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(DECISIONS_WIDTH)]).areas(lower);
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
    draw_events(frame, app, events);
    draw_decisions(frame, app, decisions);
    draw_footer(frame, app, footer);
    if let Some(prompt) = &app.prompt {
        draw_prompt(frame, prompt);
    }
}

/// A box in the middle of `area`, `width` columns and `height` rows at most.
pub(super) fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

fn draw_events(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let focused = app.focus == Focus::Events;
    let title = if app.filter.is_empty() {
        "Events".to_string()
    } else {
        format!("Events · node {}", app.filter)
    };
    let muted = Style::default().fg(p.text_muted);
    let height = area.height.saturating_sub(2) as usize;
    let width = area.width.saturating_sub(2) as usize;
    let (events, error) = app.shown_events();
    let mut lines: Vec<Line> = Vec::new();
    if app.selected_execution().is_none() {
        lines.push(Line::styled("select an execution", muted));
    } else if let Some(error) = error {
        lines.push(Line::styled(describe(error), Style::default().fg(p.danger)));
    } else if events.is_empty() {
        lines.push(Line::styled("no events yet", muted));
    } else {
        let max = app.options.tuning.value_max.min(width.max(1) * 4);
        let end = events.len().saturating_sub(app.event_offset);
        let start = end.saturating_sub(height);
        for event in &events[start..end] {
            let color = match event.kind {
                regulus_proto::BusEventKind::Failed { .. }
                | regulus_proto::BusEventKind::Cut { .. } => p.danger,
                regulus_proto::BusEventKind::Delivered { .. } => p.text_muted,
                regulus_proto::BusEventKind::Emitted { .. } => p.text,
            };
            lines.push(Line::styled(
                event_line(event, max),
                Style::default().fg(color),
            ));
        }
    }
    frame.render_widget(Paragraph::new(lines).block(pane(&title, focused, p)), area);
}

fn draw_decisions(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let focused = app.focus == Focus::Decisions;
    let muted = Style::default().fg(p.text_muted);
    let block = pane("Decisions", focused, p);
    let message = match app.decisions_of_selected() {
        _ if app.selected_execution().is_none() => Some(Line::styled("select an execution", muted)),
        None => Some(Line::styled("loading…", muted)),
        Some(Err(failure)) => Some(Line::styled(
            describe(failure),
            Style::default().fg(p.text_muted),
        )),
        Some(Ok(list)) if list.is_empty() => Some(Line::styled("nothing waits for you", muted)),
        Some(Ok(_)) => None,
    };
    if let Some(line) = message {
        frame.render_widget(Paragraph::new(line).block(block), area);
        return;
    }
    let width = area.width.saturating_sub(4) as usize;
    let max = app.options.tuning.value_max.min(width.max(1));
    let items: Vec<ListItem> = app
        .pending()
        .iter()
        .map(|pending| {
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled("? waiting ", Style::default().fg(p.warning)),
                    Span::raw(format!("{} · {}", pending.node_id, pending.kind)),
                ]),
                Line::styled(
                    format!("  {}", super::app::compact(&pending.value, max)),
                    Style::default().fg(p.text_secondary),
                ),
                Line::styled(format!("  {}", pending.prompt), muted),
            ])
        })
        .collect();
    let mut state = ListState::default().with_selected(app.decision_index());
    frame.render_stateful_widget(list(items, block, p), area, &mut state);
}

fn draw_prompt(frame: &mut Frame, prompt: &Prompt) {
    let p = palette();
    let area = centered(frame.area(), 70, 12);
    frame.render_widget(Clear, area);
    let muted = Style::default().fg(p.text_muted);
    let (title, mut lines, typed) = match prompt {
        Prompt::NewProfile(name) => (
            "New profile",
            vec![Line::styled("its name; the id comes from it", muted)],
            name.clone(),
        ),
        Prompt::Filter(text) => (
            "Filter events by node",
            vec![Line::styled("a node id; empty shows every event", muted)],
            text.clone(),
        ),
        Prompt::Send {
            ports,
            index,
            value,
            ..
        } => {
            let mut lines = vec![Line::styled(
                "↑↓ input · text@ takes text, the rest JSON",
                muted,
            )];
            for (at, port) in ports.iter().enumerate() {
                let mark = if at == *index { "▸ " } else { "  " };
                lines.push(Line::raw(format!(
                    "{mark}self.{} ({})",
                    port.id, port.capability
                )));
            }
            ("Send to the workflow", lines, value.clone())
        }
        Prompt::Bind(picker) => {
            let mut lines = vec![Line::styled(
                "↑↓ choose · ←→ inputs/outputs · Enter talk through them · x forget · Esc",
                muted,
            )];
            let rows = picker.inputs.len().max(picker.outputs.len());
            for row in 0..rows {
                let cell = |list: &[String], at: usize, column: usize| match list.get(row) {
                    Some(id) => {
                        let mark = match (row == at, picker.column == column) {
                            (true, true) => "▸ ",
                            (true, false) => "• ",
                            _ => "  ",
                        };
                        format!("{mark}self.{id}")
                    }
                    None => String::new(),
                };
                lines.push(Line::raw(format!(
                    "{:<32}{}",
                    cell(&picker.inputs, picker.input, 0),
                    cell(&picker.outputs, picker.output, 1)
                )));
            }
            let title = "Talk to the workflow: text in → text out";
            frame.render_widget(Paragraph::new(lines).block(pane(title, true, p)), area);
            return;
        }
        Prompt::Permission(picker) => {
            let mut lines = vec![
                Line::raw(format!("{} · {}", picker.request, picker.title)),
                Line::styled("↑↓ choose · Enter answer · Esc", muted),
            ];
            for (at, choice) in picker.choices.iter().enumerate() {
                let mark = if at == picker.index { "▸ " } else { "  " };
                let label = match choice {
                    PickChoice::Option(option) => {
                        format!("{} ({}, {})", option.name, option.id, option.kind)
                    }
                    PickChoice::Reject => "Reject".to_string(),
                };
                lines.push(Line::raw(format!("{mark}{label}")));
            }
            frame.render_widget(
                Paragraph::new(lines).block(pane("Answer the permission request", true, p)),
                area,
            );
            return;
        }
    };
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("› ", Style::default().fg(p.focus)),
        Span::raw(typed),
    ]));
    frame.render_widget(Paragraph::new(lines).block(pane(title, true, p)), area);
}

pub(super) fn pane<'a>(title: &'a str, focused: bool, p: &Palette) -> Block<'a> {
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
pub(super) fn state_span(
    symbol: &str,
    label: String,
    color: ratatui::style::Color,
) -> Span<'static> {
    Span::styled(format!("{symbol} {label}"), Style::default().fg(color))
}

pub(super) fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
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

pub(super) fn list<'a>(items: Vec<ListItem<'a>>, block: Block<'a>, p: &Palette) -> List<'a> {
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
        ExecutionState::Interrupted { .. } => state_span("◌", "interrupted".into(), p.warning),
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
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
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
    let workflow_talk = app.workflow_talk();
    let target = if workflow_talk.is_some() {
        None
    } else {
        app.talk_target()
    };
    let typing = app.focus == Focus::Input;
    let (body, input) = if target.is_some() || workflow_talk.is_some() {
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
        _ if workflow_talk.is_some() => conversation(app, width),
        (Some(node), _) if node.kind == NodeKind::Agent && app.agent_shown().is_some() => {
            conversation(app, width)
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

    if let Some(input) = input {
        let hint = match target {
            _ if workflow_talk.is_some() => "message to the workflow",
            Some((_, NodeKind::Agent)) => "prompt",
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

/// The conversation shown as cards (docs/04-client.md §7): a direct agent's history, or
/// a bound workflow's bus. The same reduction the app makes.
fn conversation(app: &App, width: usize) -> (String, Vec<Line<'static>>) {
    let p = palette();
    let muted = Style::default().fg(p.text_muted);
    let max = app.options.tuning.value_max.min(width.max(1) * 4);
    let Some((records, grouping, agent)) = app.shown_records() else {
        return (String::new(), Vec::new());
    };
    let mut lines: Vec<Line<'static>> = Vec::new();
    let (title, speaker, log) = match app.workflow_talk() {
        Some((_, binding, source)) => {
            let from = match source {
                BindingSource::Own => "",
                BindingSource::Config => " (config.toml)",
            };
            (
                format!(
                    "self.{} → self.{}{from} · workflow",
                    binding.input, binding.output
                ),
                format!("self.{}", binding.output),
                None,
            )
        }
        None => {
            let node = agent.clone().unwrap_or_default();
            let log = app.agent_shown().and_then(|key| app.agents.get(&key));
            (format!("@{node} · agent"), node, log)
        }
    };
    if let Some(log) = log {
        let session = &log.session;
        let resume = if session["native_resume"].as_bool() == Some(true) {
            "resumed"
        } else {
            "not resumed"
        };
        lines.push(Line::styled(
            format!(
                "session {} · {} · {resume}",
                session["engine_session_id"].as_str().unwrap_or("-"),
                log.state()
            ),
            muted,
        ));
        if let Some(error) = &log.error {
            lines.push(Line::styled(describe(error), Style::default().fg(p.danger)));
        }
    }
    let who = |node: &Option<String>| {
        node.as_deref()
            .map(|n| format!("[{n}] "))
            .unwrap_or_default()
    };
    for turn in reduce(&records, grouping) {
        for item in turn.items {
            match item {
                Item::Entry { role, text, .. } => {
                    let (name, color) = match role.as_str() {
                        USER => ("you".to_string(), p.focus),
                        AGENT => (speaker.clone(), p.info),
                        SYSTEM => ("system".to_string(), p.danger),
                        other => (other.to_string(), p.text_secondary),
                    };
                    lines.extend(said(&name, &text, color, width));
                }
                Item::Live { node, text, .. } => {
                    let name = node.clone().unwrap_or_else(|| speaker.clone());
                    lines.extend(said(&format!("{name} (writing)"), &text, p.warning, width));
                }
                Item::Thought { node, text, .. } => lines.push(Line::styled(
                    format!(
                        "  {}thinking: {}",
                        who(&node),
                        compact(&Value::from(text), max)
                    ),
                    muted,
                )),
                Item::Tool {
                    node,
                    title,
                    tool_kind,
                    status,
                    content,
                    ..
                } => {
                    let (symbol, color) = match status.as_str() {
                        "completed" => ("✓", p.success),
                        "failed" => ("✗", p.danger),
                        _ => ("…", p.warning),
                    };
                    let kind = tool_kind.map(|k| format!(" ({k})")).unwrap_or_default();
                    lines.push(Line::from(vec![
                        Span::raw(format!("  {}⚙ {title}{kind} ", who(&node))),
                        state_span(symbol, status, color),
                    ]));
                    if let Some(content) = content {
                        lines.push(Line::styled(
                            format!("    {}", compact(&content, max)),
                            muted,
                        ));
                    }
                }
                Item::Plan { node, entries, .. } => lines.push(Line::styled(
                    format!("  {}plan: {}", who(&node), compact(&entries, max)),
                    muted,
                )),
                Item::Permission { request, resolved } => {
                    let options: Vec<&str> =
                        request.options.iter().map(|o| o.id.as_str()).collect();
                    let line = match resolved {
                        None => state_span(
                            "?",
                            format!(
                                "{}permission: {} [{}] · {} answers",
                                who(&request.node),
                                request.title,
                                options.join(" | "),
                                app.options.keys.answer.label()
                            ),
                            p.warning,
                        ),
                        Some(Some(option)) => state_span(
                            "✓",
                            format!(
                                "{}permission: {} → {option}",
                                who(&request.node),
                                request.title
                            ),
                            p.success,
                        ),
                        Some(None) => state_span(
                            "✗",
                            format!(
                                "{}permission: {} cancelled",
                                who(&request.node),
                                request.title
                            ),
                            p.text_muted,
                        ),
                    };
                    lines.push(Line::from(vec![Span::raw("  "), line]));
                }
                Item::Ended {
                    node, stop_reason, ..
                } => lines.push(Line::styled(
                    format!("  {}turn ended ({stop_reason})", who(&node)),
                    muted,
                )),
                Item::State { node, state, .. } => lines.push(Line::styled(
                    format!("  {}state {state}", who(&node)),
                    muted,
                )),
                Item::Other {
                    node, name, raw, ..
                } => lines.push(Line::styled(
                    format!("  {}{name} {}", who(&node), compact(&raw, max)),
                    muted,
                )),
            }
        }
        if turn.open {
            lines.push(Line::styled(
                format!("{speaker} is working…"),
                Style::default().fg(p.warning),
            ));
        }
        lines.push(Line::raw(""));
    }
    for (_, text) in log.map(|l| l.queued.as_slice()).unwrap_or_default() {
        lines.extend(said("you (queued)", text, p.text_muted, width));
    }
    if records.is_empty() && log.is_none_or(|l| l.queued.is_empty()) {
        lines.push(Line::styled("press Enter and write", muted));
    }
    (title, lines)
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let k = &app.options.keys;
    let keys = match app.focus {
        Focus::Input => format!(
            "Enter send · {} cancel turn · Esc back · Ctrl-C quit",
            k.cancel_turn.label()
        ),
        Focus::Profiles => format!(
            "Enter run · {} edit · {} new · {} templates · Tab next pane · r refresh · q quit",
            k.edit.label(),
            k.new_profile.label(),
            k.templates.label()
        ),
        Focus::Executions => format!(
            "s stop · Enter nodes · {} send · {} templates · Tab next pane · q quit",
            k.send.label(),
            k.templates.label()
        ),
        Focus::Nodes => format!(
            "Enter write · {} talk to the workflow · {} answer permission · {} cancel turn · \
             ↑↓ move · Tab next pane · q quit",
            k.talk.label(),
            k.answer.label(),
            k.cancel_turn.label()
        ),
        Focus::Events => format!(
            "↑↓ scroll · {} filter by node · {} send · Tab next pane · q quit",
            k.filter.label(),
            k.send.label()
        ),
        Focus::Decisions => format!(
            "{} approve · {} reject · ↑↓ move · Tab next pane · q quit",
            k.approve.label(),
            k.reject.label()
        ),
    };
    let line = notice_line(app, keys);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(p.sidebar)),
        area,
    );
}

/// The notice if there is one, else the keys of where the person is.
pub(super) fn notice_line(app: &App, keys: String) -> Line<'static> {
    let p = palette();
    match &app.notice {
        Some((Tone::Error, text)) if text.starts_with(['✗', '□']) => {
            Line::styled(format!(" {text}"), Style::default().fg(p.danger))
        }
        Some((Tone::Error, text)) => {
            Line::styled(format!(" ✗ {text}"), Style::default().fg(p.danger))
        }
        Some((Tone::Info, text)) => Line::styled(format!(" {text}"), Style::default().fg(p.info)),
        None => Line::styled(format!(" {keys}"), Style::default().fg(p.text_muted)),
    }
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
    fn an_agent_conversation_is_drawn_as_cards() {
        use super::super::core::Event;
        let mut app = App::default();
        let profile: regulus_proto::Profile = serde_json::from_value(serde_json::json!({
            "id": "p", "main_workflow_id": "main", "workflows": [{ "id": "main", "kind": "main",
            "nodes": [{ "id": "dev", "type": "agent", "module_id": "m/agent@1", "ports": [
                { "id": "engine", "capability": "agent.engine@1", "direction": "input",
                  "kind": "uses" }] }] }]
        }))
        .unwrap();
        let execution: regulus_proto::Execution = serde_json::from_value(serde_json::json!({
            "id": "exec-1", "profile_id": "p", "workflow_id": "main",
            "state": { "status": "running" }, "disabled": []
        }))
        .unwrap();
        let module: regulus_proto::ModuleSummary = serde_json::from_value(serde_json::json!({
            "id": "m/agent@1", "name": "agent", "node_types": [],
            "state": { "status": "active", "since": 0 }
        }))
        .unwrap();
        app.apply(Event::Profiles(vec![profile]));
        app.apply(Event::Status {
            executions: vec![execution],
            modules: vec![module],
        });
        app.apply(Event::AgentHistory {
            execution: "exec-1".into(),
            node: "dev".into(),
            since: 0,
            result: Ok(serde_json::json!({
                "entries": [{ "seq": 1, "role": "user", "text": "hi", "turn": "t-1" }],
                "activity": [
                    { "seq": 2, "turn": "t-1", "event": { "kind": "tool_call", "id": "c1",
                      "title": "read a file", "status": "pending" } },
                    { "seq": 3, "turn": "t-1", "event": { "kind": "permission_request",
                      "id": "p1", "title": "write", "options": [
                        { "id": "yes", "kind": "allow_once", "name": "Allow" }] } }],
                "next": 3, "session": { "state": "busy" }
            })),
        });
        let shown = screen(&app);
        assert!(shown.contains("@dev · agent"), "{shown}");
        assert!(shown.contains("you › hi"), "{shown}");
        assert!(shown.contains("⚙ read a file"), "{shown}");
        assert!(
            shown.contains("? permission: write [yes] · a answers"),
            "{shown}"
        );
        assert!(shown.contains("dev is working…"), "{shown}");
    }

    #[test]
    fn long_text_wraps_to_the_width() {
        assert_eq!(wrap("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(wrap("one two three", 8), vec!["one two", "three"]);
        assert_eq!(wrap("", 4), vec![""]);
        assert_eq!(wrap("a\n\nb", 4), vec!["a", "", "b"]);
    }
}
