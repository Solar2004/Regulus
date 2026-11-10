//! Drawing the coding screen (docs/04-client.md §12): the conversation with the agent in
//! the middle, a box to type in under it, the session and its workspace in a side panel,
//! and the keys at the bottom. Nothing is decided here: it draws what `app.rs` holds.
//!
//! The transcript is laid out once per change and width, not per frame: a long
//! conversation costs the same to redraw as a short one.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use regulus_proto::{ExecutionState, ExecutionTree};
use serde_json::Value;

use super::app::{App, Chat, Focus, Overlay, PickRow, Setup, Stage, Tab};
use super::commands::COMMANDS;
use super::inspect::Summary;
use super::mascot::{self, Mood};
use super::session::model_of;
use super::settings::StatusField;
use crate::conversation::{reduce, Grouping, Item, TurnView, AGENT, SYSTEM, USER};
use crate::tui::app::{compact, NodeState, Tone};
use crate::tui::core::{describe, Connection};
use crate::tui::keys::Keys;
use crate::tui::palette::{palette, Palette};
use crate::tui::talk::Choice;
use crate::tui::templates::FormRow;
use crate::tui::view::{centered, pane, state_span};

/// Columns kept free at each side of the transcript.
const MARGIN: u16 = 1;
/// How far the continuation of a card is set in.
const INDENT: &str = "  ";
/// Columns a tab in shown text takes.
const TAB: &str = "    ";
/// The widest the centred boxes (setup, help, sessions) get.
const BOX_WIDTH: u16 = 84;

/// What the screen keeps between frames.
#[derive(Default)]
pub struct Cache {
    /// What the transcript below was laid out for: the execution, the conversation's
    /// version and the width.
    key: Option<(String, u64, usize)>,
    turns: Vec<TurnView>,
    lines: Vec<Line<'static>>,
    /// How far the transcript and the side panel could scroll when last drawn, and how
    /// many rows a page is: the app holds its scrolls inside these.
    pub transcript_room: usize,
    pub side_room: usize,
    pub page: usize,
}

// ── Text ──────────────────────────────────────────────────────────────────────────────

/// Text safe to put on screen: tabs as spaces and nothing that moves a terminal's cursor.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\t' => out.push_str(TAB),
            '\n' => out.push('\n'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// A line cut into pieces of `width` characters, keeping every space: for code.
fn chop(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

/// A line wrapped between words to `width` characters; what does not fit goes on under
/// the line's own indentation, so lists and nested text keep their shape.
fn wrap(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let indent: String = line
        .chars()
        .take_while(|c| *c == ' ')
        .take(width / 2)
        .collect();
    let room = width - indent.chars().count();
    let mut out = Vec::new();
    let mut current = String::new();
    let mut used = 0;
    for word in line.trim_start_matches(' ').split(' ') {
        let mut word: Vec<char> = word.chars().collect();
        let gap = usize::from(used > 0);
        if used > 0 && used + gap + word.len() > room {
            out.push(format!("{indent}{current}"));
            current.clear();
            used = 0;
        } else if gap == 1 {
            current.push(' ');
            used += 1;
        }
        while word.len() > room {
            let rest = word.split_off(room);
            out.push(format!("{indent}{}", word.iter().collect::<String>()));
            word = rest;
        }
        used += word.len();
        current.extend(word);
    }
    out.push(format!("{indent}{current}"));
    out
}

/// Characters past `max` become `…`.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() > max.max(1) {
        let kept: String = text.chars().take(max.max(1).saturating_sub(1)).collect();
        format!("{kept}…")
    } else {
        text.to_string()
    }
}

/// `first` before the first line and as much space before the rest.
fn hang(first: Span<'static>, rows: Vec<Line<'static>>) -> Vec<Line<'static>> {
    let pad = " ".repeat(first.width());
    rows.into_iter()
        .enumerate()
        .map(|(at, row)| {
            let head = if at == 0 {
                first.clone()
            } else {
                Span::raw(pad.clone())
            };
            let mut spans = vec![head];
            spans.extend(row.spans);
            Line::from(spans)
        })
        .collect()
}

/// What an agent wrote, with the little of Markdown that matters in a terminal: code
/// between fences keeps its spaces and is cut, not wrapped; a heading is bold.
fn prose(text: &str, width: usize, style: Style, p: &Palette) -> Vec<Line<'static>> {
    let code = Style::default().fg(p.text_secondary);
    let fence = Style::default().fg(p.text_muted);
    let mut lines = Vec::new();
    let mut fenced = false;
    for line in clean(text).lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            lines.push(Line::styled(cut(line, width), fence));
        } else if fenced {
            lines.extend(chop(line, width).into_iter().map(|l| Line::styled(l, code)));
        } else {
            let style = if line.starts_with('#') {
                style.add_modifier(Modifier::BOLD)
            } else {
                style
            };
            lines.extend(
                wrap(line, width)
                    .into_iter()
                    .map(|l| Line::styled(l, style)),
            );
        }
    }
    if lines.is_empty() {
        lines.push(Line::raw(""));
    }
    lines
}

/// How a tool call or a plan entry stands: a symbol and a word, never color alone.
fn progress(status: &str, p: &Palette) -> (&'static str, Color) {
    match status {
        "completed" => ("✓", p.success),
        "failed" => ("✗", p.danger),
        "in_progress" => ("…", p.warning),
        _ => ("·", p.text_muted),
    }
}

/// What a tool gave back as text: text as it is, anything else as JSON.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => clean(text),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match &part["text"] {
                Value::String(text) => clean(text),
                _ => part.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    }
}

fn plan_lines(entries: &Value, width: usize, p: &Palette) -> Vec<Line<'static>> {
    entries
        .as_array()
        .into_iter()
        .flatten()
        .map(|entry| {
            let status = entry["status"].as_str().unwrap_or("pending");
            let (symbol, color) = progress(status, p);
            let text = entry["content"]
                .as_str()
                .map(clean)
                .unwrap_or_else(|| entry.to_string());
            let room = width.saturating_sub(4);
            Line::from(vec![
                Span::styled(format!("{INDENT}{symbol} "), Style::default().fg(color)),
                Span::styled(cut(&text, room), Style::default().fg(p.text_secondary)),
            ])
        })
        .collect()
}

// ── The transcript ────────────────────────────────────────────────────────────────────

/// The cards of one item of a turn.
fn item_lines(
    item: &Item,
    width: usize,
    tool_lines: usize,
    keys: &Keys,
    p: &Palette,
) -> Vec<Line<'static>> {
    let muted = Style::default().fg(p.text_muted);
    let text = Style::default().fg(p.text);
    let inner = width.saturating_sub(INDENT.len());
    let who = |node: &Option<String>| {
        node.as_deref()
            .map(|node| format!("[{node}] "))
            .unwrap_or_default()
    };
    match item {
        Item::Entry {
            role, text: said, ..
        } if role == USER => hang(
            Span::styled(
                "› ",
                Style::default().fg(p.focus).add_modifier(Modifier::BOLD),
            ),
            prose(said, inner, text.add_modifier(Modifier::BOLD), p),
        ),
        Item::Entry {
            role, text: said, ..
        } if role == AGENT => hang(
            Span::styled("✦ ", Style::default().fg(p.steel)),
            prose(said, inner, text, p),
        ),
        Item::Entry {
            role, text: said, ..
        } => {
            let (mark, color) = if role == SYSTEM {
                ("! ", p.danger)
            } else {
                ("· ", p.text_secondary)
            };
            hang(
                Span::styled(mark, Style::default().fg(color)),
                prose(said, inner, Style::default().fg(color), p),
            )
        }
        Item::Live {
            node, text: said, ..
        } => {
            let mut lines = hang(
                Span::styled("✦ ", Style::default().fg(p.warning)),
                prose(&format!("{}{said}", who(node)), inner, text, p),
            );
            lines.push(Line::styled(format!("{INDENT}… writing"), muted));
            lines
        }
        Item::Thought {
            node, text: said, ..
        } => {
            let line = compact(&Value::from(clean(said)), inner.saturating_sub(12).max(8));
            vec![Line::styled(
                format!("{INDENT}~ {}thinking: {line}", who(node)),
                muted.add_modifier(Modifier::ITALIC),
            )]
        }
        Item::Tool {
            node,
            title,
            tool_kind,
            status,
            content,
            ..
        } => {
            let (symbol, color) = progress(status, p);
            let word = if status.is_empty() { "pending" } else { status };
            let kind = tool_kind
                .as_deref()
                .map(|kind| format!(" ({kind})"))
                .unwrap_or_default();
            let room = inner.saturating_sub(word.len() + kind.len() + 6);
            let mut lines = vec![Line::from(vec![
                Span::styled(format!("{INDENT}{symbol} "), Style::default().fg(color)),
                Span::styled(
                    format!("{}{}", who(node), cut(&clean(title), room)),
                    Style::default().fg(p.text_secondary),
                ),
                Span::styled(kind, muted),
                Span::styled(format!(" · {word}"), Style::default().fg(color)),
            ])];
            if let Some(content) = content.as_ref().filter(|_| tool_lines > 0) {
                let said = content_text(content);
                let all: Vec<&str> = said.lines().collect();
                for line in all.iter().take(tool_lines) {
                    lines.push(Line::styled(
                        format!("{INDENT}  │ {}", cut(line, inner.saturating_sub(6))),
                        muted,
                    ));
                }
                if all.len() > tool_lines {
                    lines.push(Line::styled(
                        format!("{INDENT}  │ … {} more lines", all.len() - tool_lines),
                        muted,
                    ));
                }
            }
            lines
        }
        Item::Plan { node, entries, .. } => {
            let mut lines = vec![Line::styled(format!("{INDENT}◇ {}plan", who(node)), muted)];
            lines.extend(plan_lines(entries, inner, p).into_iter().map(|line| {
                let mut spans = vec![Span::raw(INDENT)];
                spans.extend(line.spans);
                Line::from(spans)
            }));
            lines
        }
        Item::Permission { request, resolved } => {
            let tool = request
                .tool
                .as_deref()
                .map(|tool| format!(" ({tool})"))
                .unwrap_or_default();
            let asked = format!("{}{}{tool}", who(&request.node), clean(&request.title));
            let (symbol, said, color) = match resolved {
                None => (
                    "?",
                    format!(
                        "permission: {asked} · waiting for you ({})",
                        keys.code_permission.label()
                    ),
                    p.warning,
                ),
                Some(Some(option)) => {
                    let name = request
                        .options
                        .iter()
                        .find(|o| o.id == *option)
                        .map_or(option.as_str(), |o| o.name.as_str());
                    ("✓", format!("permission: {asked} → {name}"), p.success)
                }
                Some(None) => (
                    "✗",
                    format!("permission: {asked} · cancelled"),
                    p.text_muted,
                ),
            };
            hang(
                Span::styled(format!("{INDENT}{symbol} "), Style::default().fg(color)),
                wrap(&said, inner.saturating_sub(2))
                    .into_iter()
                    .map(|l| Line::styled(l, Style::default().fg(color)))
                    .collect(),
            )
        }
        // A turn that ended as turns do says nothing; one that did not says how.
        Item::Ended { stop_reason, .. } if stop_reason == "end_turn" => Vec::new(),
        Item::Ended {
            node, stop_reason, ..
        } => vec![Line::styled(
            format!("{INDENT}◦ {}turn ended ({stop_reason})", who(node)),
            muted,
        )],
        // The session's state is in the header.
        Item::State { .. } => Vec::new(),
        Item::Other {
            node, name, raw, ..
        } => vec![Line::styled(
            format!(
                "{INDENT}· {}{name} {}",
                who(node),
                compact(raw, inner.saturating_sub(name.len() + 4).max(8))
            ),
            muted,
        )],
    }
}

/// The conversation as lines `width` wide: each turn's cards, a blank line between turns,
/// and at the end what was sent and the box has not recorded yet.
fn transcript(
    turns: &[TurnView],
    chat: &Chat,
    width: usize,
    tool_lines: usize,
    keys: &Keys,
) -> Vec<Line<'static>> {
    let p = palette();
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(error) = &chat.log.error {
        lines.push(Line::styled(
            format!("the conversation cannot be read: {}", describe(error)),
            Style::default().fg(p.danger),
        ));
        lines.push(Line::raw(""));
    }
    for turn in turns {
        let before = lines.len();
        for item in &turn.items {
            lines.extend(item_lines(item, width, tool_lines, keys, p));
        }
        let writing = turn.items.iter().any(|i| matches!(i, Item::Live { .. }));
        if turn.open && !writing {
            lines.push(Line::styled(
                format!("{INDENT}… working"),
                Style::default().fg(p.warning),
            ));
        }
        if lines.len() > before {
            lines.push(Line::raw(""));
        }
    }
    for (_, text) in &chat.log.queued {
        lines.extend(hang(
            Span::styled("› ", Style::default().fg(p.text_muted)),
            prose(
                text,
                width.saturating_sub(INDENT.len()),
                Style::default().fg(p.text_muted),
                p,
            ),
        ));
        lines.push(Line::styled(
            format!("{INDENT}… queued"),
            Style::default().fg(p.text_muted),
        ));
        lines.push(Line::raw(""));
    }
    lines
}

/// What an empty conversation shows: who is here, where, and what holds for the session.
fn welcome(app: &App, chat: &Chat, width: usize) -> Vec<Line<'static>> {
    let p = palette();
    let muted = Style::default().fg(p.text_muted);
    let folder = folder(chat).unwrap_or_else(|| "no workspace".to_string());
    let model = chat
        .session
        .workflow()
        .and_then(|workflow| model_of(workflow, &[]))
        .map_or_else(
            || "the harness's model".to_string(),
            |m| format!("model {m}"),
        );
    let facts = [
        Line::styled(
            app.settings.label.clone(),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            cut(&folder, width.saturating_sub(12)),
            Style::default().fg(p.text_secondary),
        ),
        Line::styled(cut(&model, width.saturating_sub(12)), muted),
    ];
    let mut lines = vec![Line::raw("")];
    if app.settings.mascot && width > 24 {
        let beat = mascot::beat(app.now, app.options.tuning.code_twinkle_ms);
        for (face, fact) in mascot::lines(app.mood(), beat, p).into_iter().zip(facts) {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(face.spans);
            spans.push(Span::raw("   "));
            spans.extend(fact.spans);
            lines.push(Line::from(spans));
        }
    } else {
        lines.extend(facts);
    }
    lines.push(Line::raw(""));
    let keys = &app.options.keys;
    let said = [
        format!(
            "Say what you want done and press {}. /help lists the commands, {} the keys.",
            keys.code_send.label(),
            keys.code_help.label()
        ),
        "When the agent needs your permission, a bar opens above this box to answer it."
            .to_string(),
        "Commands it runs start in this folder. They are not sandboxed.".to_string(),
        "What you read here is Regulus' record of the conversation. A new session starts \
         the model without the context of earlier ones."
            .to_string(),
    ];
    for line in said {
        lines.extend(
            wrap(&line, width)
                .into_iter()
                .map(|l| Line::styled(l, muted)),
        );
    }
    lines
}

// ── What a session is, in words ───────────────────────────────────────────────────────

/// The folder the session works in: what its workspace answered, else what it is set to.
fn folder(chat: &Chat) -> Option<String> {
    match &chat.side.summary {
        Some(Ok(summary)) if !summary.root.is_empty() => Some(summary.root.clone()),
        _ => chat.session.folder().map(str::to_string),
    }
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

/// How the agent stands for the person: a symbol and a word.
fn agent_state(chat: &Chat, p: &Palette) -> Span<'static> {
    if !chat.session.execution.state.is_live() {
        return execution_state(&chat.session.execution.state, p);
    }
    match chat.agent_state() {
        Some(NodeState::Disabled(missing)) => {
            return state_span(
                "✗",
                format!("disabled: needs {}", missing.join(", ")),
                p.danger,
            )
        }
        Some(NodeState::NotInstalled) => {
            return state_span("□", "not installed".into(), p.text_muted)
        }
        Some(NodeState::Broken(_)) => return state_span("▲", "broken".into(), p.danger),
        _ => {}
    }
    if !chat.open_requests().is_empty() {
        state_span("?", "waiting for you".into(), p.warning)
    } else if chat.busy() {
        state_span("…", "working".into(), p.warning)
    } else {
        state_span("✓", "ready".into(), p.success)
    }
}

fn connection_state(connection: &Connection, p: &Palette) -> Span<'static> {
    match connection {
        Connection::Connecting => state_span("…", "connecting".into(), p.warning),
        Connection::Starting => state_span("…", "starting the core".into(), p.warning),
        Connection::Connected { core, proto } => {
            state_span("✓", format!("{core} · proto {proto}"), p.success)
        }
        Connection::Failed(_) => state_span("✗", "no core".into(), p.danger),
    }
}

/// Git in a few words: the branch and how much changed, or why it says nothing.
fn git_line(summary: &Summary) -> String {
    match (&summary.git.unavailable, &summary.git.branch) {
        (Some(why), _) => format!("git: {why}"),
        (None, branch) => {
            let branch = branch.as_deref().unwrap_or("detached");
            match summary.git.changes.len() {
                0 => format!("⎇ {branch} · clean"),
                1 => format!("⎇ {branch} · 1 change"),
                n => format!("⎇ {branch} · {n} changes"),
            }
        }
    }
}

// ── The screen ────────────────────────────────────────────────────────────────────────

pub fn draw(frame: &mut Frame, app: &App, cache: &mut Cache) {
    let p = palette();
    let area = frame.area();
    frame.render_widget(
        Block::new().style(Style::default().bg(p.canvas).fg(p.text)),
        area,
    );
    let status = app.settings.status_bar && !app.settings.status_fields.is_empty();
    let [header, status_area, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(u16::from(status)),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(frame, app, header);
    if status {
        draw_status(frame, app, status_area);
    }
    match &app.stage {
        Stage::Chat(chat) => draw_chat(frame, app, chat, body, cache),
        Stage::Setup(setup) => draw_setup(frame, app, setup, body),
        Stage::Opening => {
            let said = match &app.connection {
                Connection::Connecting => "connecting to the core",
                Connection::Starting => "starting the core",
                Connection::Connected { .. } => "looking at what there is",
                Connection::Failed(_) => "no core answered",
            };
            draw_waiting(frame, app, body, said, None);
        }
        Stage::Launching { what, .. } => draw_waiting(frame, app, body, what, None),
        Stage::Blocked(why) => draw_waiting(frame, app, body, "cannot go on", Some(why)),
    }
    draw_footer(frame, app, footer);
    match &app.overlay {
        Some(Overlay::Help) => draw_help(frame, app),
        Some(Overlay::Sessions(index)) => draw_sessions(frame, app, *index),
        Some(Overlay::ConfirmStop) => draw_confirm(
            frame,
            "Stop the execution",
            &format!(
                "Stop {}? Its agent and its workers end with it.",
                app.chat().map_or("it", |c| c.session.execution.id.as_str())
            ),
            &format!(
                "{} stop it · any other key keeps it",
                app.options.keys.yes.label()
            ),
        ),
        Some(Overlay::ConfirmLeave) => draw_confirm(
            frame,
            "Leave",
            "Stop the execution before leaving, or leave it running to come back to?",
            &format!(
                "{} stop it · {} leave it running · {} stay",
                app.options.keys.yes.label(),
                app.options.keys.no.label(),
                app.options.keys.back.label()
            ),
        ),
        None => {}
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let brand_style = Style::default()
        .fg(p.on_accent)
        .bg(p.accent)
        .add_modifier(Modifier::BOLD);
    let mut left = vec![Span::styled(" ", brand_style)];
    if app.settings.mascot {
        // The star of the one-line mascot twinkles while a turn is in flight.
        let beat = mascot::beat(app.now, app.options.tuning.code_twinkle_ms);
        let working = app.mood() == Mood::Working;
        let mut glyph = mascot::glyph(p, p.accent);
        if working && beat {
            glyph[1] = Span::styled("+", brand_style);
        }
        left.extend(glyph);
        left.push(Span::styled(" ", brand_style));
    }
    left.push(Span::styled(
        format!("{} ", app.settings.label.to_uppercase()),
        brand_style,
    ));
    let right = vec![connection_state(&app.connection, p), Span::raw(" ")];
    let width = area.width as usize;
    let right_width: usize = right.iter().map(Span::width).sum();
    // The left gives way first: the state on the right is always whole.
    let mut used = 0;
    let room = width.saturating_sub(right_width + 1);
    let mut spans = Vec::new();
    for span in left {
        let fits = room.saturating_sub(used);
        if fits == 0 {
            break;
        }
        let shown = cut(&span.content, fits);
        used += shown.chars().count();
        spans.push(Span::styled(shown, span.style));
    }
    spans.push(Span::raw(
        " ".repeat(width.saturating_sub(used + right_width)),
    ));
    spans.extend(right);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(p.sidebar)),
        area,
    );
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let Some(chat) = app.chat() else { return };
    let p = palette();
    let mut values = Vec::new();
    for field in &app.settings.status_fields {
        let value = match field {
            StatusField::Root => folder(chat).unwrap_or_else(|| "no workspace".into()),
            StatusField::Model => chat
                .session
                .workflow()
                .and_then(|workflow| model_of(workflow, &[]))
                .unwrap_or_else(|| "unknown".into()),
            StatusField::Branch => chat
                .side
                .summary
                .as_ref()
                .and_then(|summary| summary.as_ref().ok())
                .map(git_line)
                .unwrap_or_else(|| "git: reading".into()),
            StatusField::State => agent_state(chat, p).content.to_string(),
            StatusField::Session => chat.session.execution.id.clone(),
        };
        values.push(format!("{} {value}", field.name()));
    }
    let line = format!(" {}", values.join("  ·  "));
    frame.render_widget(
        Paragraph::new(cut(&line, area.width.saturating_sub(1) as usize))
            .style(Style::default().fg(p.text_secondary).bg(p.sidebar)),
        area,
    );
}

/// The mascot and a word in the middle of the screen, while there is nothing else to
/// show; `why` is what stops it.
fn draw_waiting(frame: &mut Frame, app: &App, area: Rect, said: &str, why: Option<&String>) {
    let p = palette();
    let width = area.width.saturating_sub(4).min(BOX_WIDTH) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.settings.mascot {
        let beat = mascot::beat(app.now, app.options.tuning.code_twinkle_ms);
        lines.extend(mascot::lines(app.mood(), beat, p));
        lines.push(Line::raw(""));
    }
    lines.push(Line::styled(
        app.settings.label.clone(),
        Style::default().fg(p.text).add_modifier(Modifier::BOLD),
    ));
    let color = if why.is_some() {
        p.danger
    } else {
        p.text_muted
    };
    lines.push(Line::styled(said.to_string(), Style::default().fg(color)));
    if let Some(why) = why {
        lines.push(Line::raw(""));
        for line in clean(why).lines() {
            lines.extend(
                wrap(line, width)
                    .into_iter()
                    .map(|l| Line::styled(l, Style::default().fg(p.text_secondary))),
            );
        }
    }
    if let (Connection::Failed(reason), None) = (&app.connection, why) {
        lines.push(Line::raw(""));
        lines.extend(
            wrap(reason, width)
                .into_iter()
                .map(|l| Line::styled(l, Style::default().fg(p.text_secondary))),
        );
    }
    let height = (lines.len() as u16).min(area.height);
    let place = centered(area, (width as u16).max(mascot::WIDTH), height);
    frame.render_widget(Paragraph::new(lines).centered(), place);
}

fn draw_chat(frame: &mut Frame, app: &App, chat: &Chat, area: Rect, cache: &mut Cache) {
    let tuning = &app.options.tuning;
    let asked = chat.asked();
    let asking_rows = asked
        .as_ref()
        .map_or(0, |(_, choices, _)| choices.len() as u16 + 4);
    // On a narrow terminal there is room for one of the two: the side panel takes the
    // conversation's place while it has the keys, and gives it back with them.
    let side_width = tuning.code_side_width.min(u16::MAX as usize) as u16;
    let fits = area.width as usize >= tuning.code_side_min_width && area.width > side_width + 30;
    let side = chat.side.shown && fits;
    let instead = chat.side.shown && !fits && chat.focus == Focus::Side;
    let [main, panel] = if side {
        Layout::horizontal([Constraint::Min(30), Constraint::Length(side_width)]).areas(area)
    } else {
        [area, Rect::default()]
    };
    // Inside the box: its border, a column free at each side and the `› ` it starts with.
    let text_width = main.width.saturating_sub(6).max(1) as usize;
    let rows = chat.composer.rows(text_width);
    let most = tuning.code_composer_rows.max(1) as u16;
    let composer_rows = (rows.rows.len() as u16).clamp(1, most);
    let [conversation, asking, composer] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(asking_rows.min(main.height.saturating_sub(4))),
        Constraint::Length(composer_rows + 2),
    ])
    .areas(main);
    lay_out(app, chat, conversation, cache);
    cache.side_room = 0;
    if instead {
        cache.transcript_room = usize::MAX;
        draw_side(frame, app, chat, conversation, cache);
    } else {
        draw_transcript(frame, app, chat, conversation, cache);
    }
    if let Some((request, choices, index)) = &asked {
        draw_asking(
            frame,
            app,
            chat,
            asking,
            request.title.as_str(),
            choices,
            *index,
        );
    }
    draw_composer(frame, app, chat, composer, rows, composer_rows);
    if chat.focus == Focus::Composer && chat.asking.is_none() && app.overlay.is_none() {
        draw_commands(frame, app, chat, composer, conversation);
    }
    if side {
        draw_side(frame, app, chat, panel, cache);
    }
}

/// Lays the conversation out for the place it is drawn in, if it changed or the place did.
fn lay_out(app: &App, chat: &Chat, area: Rect, cache: &mut Cache) {
    let width = area.width.saturating_sub(MARGIN * 2).max(1) as usize;
    let key = (chat.session.execution.id.clone(), chat.version, width);
    if cache.key.as_ref() != Some(&key) {
        cache.turns = reduce(&chat.log.records, Grouping::Turn);
        cache.lines = transcript(
            &cache.turns,
            chat,
            width,
            app.options.tuning.code_tool_lines,
            &app.options.keys,
        );
        cache.key = Some(key);
    }
    cache.page = (area.height as usize).saturating_sub(2).max(1);
}

fn draw_transcript(frame: &mut Frame, app: &App, chat: &Chat, area: Rect, cache: &mut Cache) {
    let width = area.width.saturating_sub(MARGIN * 2).max(1) as usize;
    let height = area.height as usize;
    let place = Rect {
        x: area.x + MARGIN,
        width: width as u16,
        ..area
    };
    if cache.lines.is_empty() {
        cache.transcript_room = 0;
        frame.render_widget(Paragraph::new(welcome(app, chat, width)), place);
        return;
    }
    let total = cache.lines.len();
    cache.transcript_room = total.saturating_sub(height);
    let back = chat.scroll.min(cache.transcript_room);
    let end = total - back;
    let start = end.saturating_sub(height);
    let mut shown: Vec<Line<'static>> = cache.lines[start..end].to_vec();
    // Scrolled back: say that there is more below, where the newest would be.
    if back > 0 {
        let p = palette();
        if let Some(last) = shown.last_mut() {
            *last = Line::styled(
                format!(
                    "↓ {back} more lines · {} goes to the newest",
                    app.options.keys.back.label()
                ),
                Style::default().fg(p.info),
            );
        }
    }
    // A short conversation sits at the bottom, next to the box it is typed in.
    let pad = height.saturating_sub(shown.len());
    let mut lines = vec![Line::raw(""); pad];
    lines.append(&mut shown);
    frame.render_widget(Paragraph::new(lines), place);
}

/// The permission request that has the keys: what is asked, the choices by number, and
/// whether anyone can be answered.
fn draw_asking(
    frame: &mut Frame,
    app: &App,
    chat: &Chat,
    area: Rect,
    title: &str,
    choices: &[Choice],
    index: usize,
) {
    if area.height < 3 {
        return;
    }
    let p = palette();
    let keys = &app.options.keys;
    let width = area.width.saturating_sub(4) as usize;
    let mut lines = vec![Line::styled(
        format!(" {}", cut(&clean(title), width)),
        Style::default().fg(p.text).add_modifier(Modifier::BOLD),
    )];
    for (at, choice) in choices.iter().enumerate() {
        let mark = if at == index { "▸" } else { " " };
        let label = match choice {
            Choice::Option(option) => format!("{} ({})", option.name, option.kind),
            Choice::Reject => "Reject".to_string(),
        };
        let style = if at == index {
            Style::default().fg(p.text).bg(p.selected)
        } else {
            Style::default().fg(p.text_secondary)
        };
        lines.push(Line::styled(
            format!(" {}", cut(&format!("{mark} {} {label}", at + 1), width)),
            style,
        ));
    }
    let hint = match &chat.decisions {
        Some(Err(failure)) => format!("nobody can be answered from here: {}", describe(failure)),
        _ => format!(
            "1-{} pick · {} allow · {} reject · {} answer · {} later",
            choices.len().min(9),
            keys.yes.label(),
            keys.no.label(),
            keys.open.label(),
            keys.back.label()
        ),
    };
    lines.push(Line::styled(
        format!(" {}", cut(&hint, width)),
        Style::default().fg(p.text_muted),
    ));
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(p.warning))
        .title(Span::styled(
            " ? Permission ",
            Style::default().fg(p.warning).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(p.surface));
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_composer(
    frame: &mut Frame,
    app: &App,
    chat: &Chat,
    area: Rect,
    rows: super::composer::Rows,
    shown: u16,
) {
    let p = palette();
    let keys = &app.options.keys;
    let typing = chat.focus == Focus::Composer && chat.asking.is_none() && app.overlay.is_none();
    let muted = Style::default().fg(p.text_muted);
    let unwritable = chat.unwritable();
    let title = match &unwritable {
        Some(_) => "read only".to_string(),
        None => format!(
            "{} send · {} new line",
            keys.code_send.label(),
            keys.code_newline.label()
        ),
    };
    // The window of rows follows the cursor.
    let shown = shown as usize;
    let top = (rows.cursor.0 + 1).saturating_sub(shown);
    let mut lines: Vec<Line<'static>> = rows
        .rows
        .iter()
        .enumerate()
        .skip(top)
        .take(shown)
        .map(|(at, row)| {
            let lead = if at == 0 { " › " } else { "   " };
            Line::from(vec![
                Span::styled(lead, Style::default().fg(p.focus)),
                Span::styled(row.clone(), Style::default().fg(p.text)),
            ])
        })
        .collect();
    if chat.composer.is_empty() {
        let hint = match &unwritable {
            Some(why) => cut(why, area.width.saturating_sub(7) as usize),
            None => "what should the agent do?".to_string(),
        };
        lines = vec![Line::from(vec![
            Span::styled(" › ", Style::default().fg(p.focus)),
            Span::styled(hint, muted),
        ])];
    }
    frame.render_widget(Paragraph::new(lines).block(pane(&title, typing, p)), area);
    if typing {
        let x = area.x + 4 + rows.cursor.1 as u16;
        let y = area.y + 1 + (rows.cursor.0 - top) as u16;
        if x < area.right() && y < area.bottom() {
            frame.set_cursor_position((x, y));
        }
    }
}

/// The commands that fit what is typed after `/`, in a box over the bottom of the
/// conversation, right above the one typed in. It holds at most as many rows as the
/// composer may (`tui.code_composer_rows`) and follows the chosen one; with no room for a
/// row, or no match, nothing is drawn.
fn draw_commands(frame: &mut Frame, app: &App, chat: &Chat, composer: Rect, above: Rect) {
    let text = chat.composer.text();
    let entries = chat.menu.entries(&text);
    let room = composer.y.saturating_sub(above.y).saturating_sub(2) as usize;
    let rows = entries
        .len()
        .min(app.options.tuning.code_composer_rows.max(1))
        .min(room);
    if rows == 0 {
        return;
    }
    let p = palette();
    let k = &app.options.keys;
    let chosen = chat.menu.selected(entries.len());
    let top = (chosen + 1).saturating_sub(rows);
    let area = Rect {
        y: composer.y - rows as u16 - 2,
        height: rows as u16 + 2,
        ..composer
    };
    let inner = area.width.saturating_sub(4) as usize;
    // The usages are one column however far the list is scrolled, up to half the box.
    let usage_width = entries
        .iter()
        .map(|spec| spec.usage().chars().count())
        .max()
        .unwrap_or(0)
        .min(inner / 2);
    let lines: Vec<Line<'static>> = entries
        .iter()
        .enumerate()
        .skip(top)
        .take(rows)
        .map(|(at, spec)| {
            let on = at == chosen;
            let row = if on {
                Style::default().bg(p.selected)
            } else {
                Style::default()
            };
            let mark = if on { " ▸ " } else { "   " };
            let usage = cut(&spec.usage(), usage_width);
            let pad = " ".repeat(usage_width.saturating_sub(usage.chars().count()) + 2);
            let mut said = spec.description.to_string();
            if !spec.aliases.is_empty() {
                let also: Vec<String> = spec.aliases.iter().map(|a| format!("/{a}")).collect();
                said = format!("{said} · {}", also.join(" "));
            }
            let room = inner.saturating_sub(3 + usage_width + 2);
            Line::from(vec![
                Span::styled(mark, Style::default().fg(p.focus)),
                Span::styled(
                    usage,
                    Style::default().fg(p.text).add_modifier(Modifier::BOLD),
                ),
                Span::raw(pad),
                Span::styled(cut(&said, room), Style::default().fg(p.text_secondary)),
            ])
            .style(row)
        })
        .collect();
    let more = if entries.len() > rows {
        format!(" · {}/{}", chosen + 1, entries.len())
    } else {
        String::new()
    };
    let title = format!(
        "Commands · ↑↓ pick · {} complete · {} close{more}",
        k.code_complete.label(),
        k.back.label()
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(pane(&cut(&title, inner), true, p)),
        area,
    );
}

// ── The side panel ────────────────────────────────────────────────────────────────────

fn heading(text: &str, p: &Palette) -> Line<'static> {
    Line::styled(
        text.to_string(),
        Style::default()
            .fg(p.text_secondary)
            .add_modifier(Modifier::BOLD),
    )
}

/// Why the workspace says nothing, when it does: no node for it, or the failure by its
/// code (not installed, unavailable), never a generic error.
fn workspace_trouble<T>(chat: &Chat, loaded: &super::app::Loaded<T>) -> Option<String> {
    if chat.session.workspace.is_none() {
        return Some(
            "no workspace node in this workflow (a Uses output `code.workspace`)".to_string(),
        );
    }
    match loaded {
        None => Some("reading…".to_string()),
        Some(Err(failure)) => Some(describe(failure)),
        Some(Ok(_)) => None,
    }
}

fn activity_lines(app: &App, chat: &Chat, turns: &[TurnView], width: usize) -> Vec<Line<'static>> {
    let p = palette();
    let muted = Style::default().fg(p.text_muted);
    let plain = Style::default().fg(p.text_secondary);
    let execution = &chat.session.execution;
    let session = &chat.log.session;
    let mut lines = vec![
        heading("Session", p),
        Line::from(vec![
            Span::raw(" "),
            execution_state(&execution.state, p),
            Span::styled(format!(" · {}", execution.id), plain),
        ]),
        Line::styled(
            cut(
                &format!(" {} / {}", execution.profile_id, execution.workflow_id),
                width,
            ),
            muted,
        ),
        Line::from(vec![
            Span::styled(format!(" @{} · ", chat.session.agent), plain),
            agent_state(chat, p),
        ]),
    ];
    // The engine's session is the model's context; the transcript is Regulus' record.
    let resumed = if session["native_resume"].as_bool() == Some(true) {
        "resumed by the engine"
    } else {
        "not resumed"
    };
    lines.push(Line::styled(
        cut(
            &format!(
                " engine session {} · {resumed}",
                session["engine_session_id"].as_str().unwrap_or("-")
            ),
            width,
        ),
        muted,
    ));
    if let Some(model) = chat.session.workflow().and_then(|w| model_of(w, &[])) {
        lines.push(Line::styled(cut(&format!(" model {model}"), width), muted));
    }
    lines.push(Line::raw(""));
    lines.push(heading("Workspace", p));
    match workspace_trouble(chat, &chat.side.summary) {
        Some(trouble) => lines.extend(
            wrap(&format!(" {trouble}"), width)
                .into_iter()
                .map(|l| Line::styled(l, muted)),
        ),
        None => {
            if let Some(Ok(summary)) = &chat.side.summary {
                lines.push(Line::styled(
                    cut(&format!(" {}", summary.root), width),
                    plain,
                ));
                lines.push(Line::styled(
                    cut(&format!(" {}", git_line(summary)), width),
                    muted,
                ));
                if !summary.tools.is_empty() {
                    let tools = format!(" tools: {}", summary.tools.join(", "));
                    lines.extend(
                        wrap(&tools, width)
                            .into_iter()
                            .map(|l| Line::styled(l, muted)),
                    );
                }
            }
        }
    }
    let last = turns.last();
    let plan = last.and_then(|turn| {
        turn.items.iter().rev().find_map(|item| match item {
            Item::Plan { entries, .. } => Some(entries),
            _ => None,
        })
    });
    if let Some(entries) = plan {
        lines.push(Line::raw(""));
        lines.push(heading("Plan", p));
        lines.extend(plan_lines(entries, width, p));
    }
    let tools: Vec<&Item> = last
        .map(|turn| {
            turn.items
                .iter()
                .filter(|item| matches!(item, Item::Tool { .. }))
                .collect()
        })
        .unwrap_or_default();
    if !tools.is_empty() {
        lines.push(Line::raw(""));
        lines.push(heading("Tools of this turn", p));
        for item in tools {
            if let Item::Tool { title, status, .. } = item {
                let (symbol, color) = progress(status, p);
                lines.push(Line::from(vec![
                    Span::styled(format!(" {symbol} "), Style::default().fg(color)),
                    Span::styled(cut(&clean(title), width.saturating_sub(3)), plain),
                ]));
            }
        }
    }
    let open = chat.open_requests();
    if !open.is_empty() {
        lines.push(Line::raw(""));
        lines.push(heading("Waiting for you", p));
        for request in open {
            lines.push(Line::styled(
                cut(
                    &format!(
                        " ? {} · {}",
                        clean(&request.title),
                        app.options.keys.code_permission.label()
                    ),
                    width,
                ),
                Style::default().fg(p.warning),
            ));
        }
    }
    if !chat.notes.is_empty() {
        lines.push(Line::raw(""));
        lines.push(heading("Notes", p));
        for note in &chat.notes {
            lines.extend(hang(
                Span::styled(" · ", muted),
                wrap(note, width.saturating_sub(3))
                    .into_iter()
                    .map(|l| Line::styled(l, muted))
                    .collect(),
            ));
        }
    }
    lines
}

/// The Files tab: a file, what a search found, or the folder. Returns the lines and the
/// row the cursor is on, if it has one.
fn files_lines(chat: &Chat, width: usize) -> (Vec<Line<'static>>, Option<usize>) {
    let p = palette();
    let muted = Style::default().fg(p.text_muted);
    let plain = Style::default().fg(p.text_secondary);
    let files = &chat.side.files;
    let picked = |on: bool| {
        if on {
            Style::default().fg(p.text).bg(p.selected)
        } else {
            plain
        }
    };
    // The cursor of a list is a mark, never a color alone.
    let mark = |on: bool| if on { "▸ " } else { "  " };
    if let Some(viewer) = &files.viewer {
        let mut lines = vec![heading(&cut(&viewer.path, width), p)];
        match workspace_trouble(chat, &viewer.text) {
            Some(trouble) => lines.extend(
                wrap(&trouble, width)
                    .into_iter()
                    .map(|l| Line::styled(l, muted)),
            ),
            None => {
                if let Some(Ok(text)) = &viewer.text {
                    let body = clean(&text.text);
                    let digits = body.lines().count().max(1).to_string().len();
                    for (at, line) in body.lines().enumerate() {
                        let room = width.saturating_sub(digits + 1);
                        lines.push(Line::from(vec![
                            Span::styled(format!("{:>digits$} ", at + 1), muted),
                            Span::styled(cut(line, room), plain),
                        ]));
                    }
                    if text.truncated {
                        lines.push(Line::styled("… the rest was left out", muted));
                    }
                }
            }
        }
        return (lines, None);
    }
    if let Some(search) = &files.search {
        if search.typing {
            let lines = vec![
                heading("Search", p),
                Line::from(vec![
                    Span::styled("/ ", Style::default().fg(p.focus)),
                    Span::styled(format!("{}▏", search.query), Style::default().fg(p.text)),
                ]),
                Line::styled("Enter searches · Esc gives up", muted),
            ];
            return (lines, None);
        }
        let mut lines = vec![heading(&cut(&format!("“{}”", search.query), width), p)];
        let head = lines.len();
        match workspace_trouble(chat, &search.found) {
            Some(trouble) => lines.extend(
                wrap(&trouble, width)
                    .into_iter()
                    .map(|l| Line::styled(l, muted)),
            ),
            None => {
                if let Some(Ok(found)) = &search.found {
                    if found.matches.is_empty() {
                        lines.push(Line::styled("no matches", muted));
                    }
                    for (at, hit) in found.matches.iter().enumerate() {
                        let on = at == search.index;
                        let said =
                            format!("{}{}:{} {}", mark(on), hit.path, hit.line, hit.text.trim());
                        lines.push(Line::styled(cut(&clean(&said), width), picked(on)));
                    }
                    if found.truncated {
                        lines.push(Line::styled("… there are more", muted));
                    }
                    return (lines, Some(head + search.index));
                }
            }
        }
        return (lines, None);
    }
    let shown = if files.path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", files.path)
    };
    let mut lines = vec![heading(&cut(&shown, width), p)];
    let head = lines.len();
    match workspace_trouble(chat, &files.listing) {
        Some(trouble) => {
            lines.extend(
                wrap(&trouble, width)
                    .into_iter()
                    .map(|l| Line::styled(l, muted)),
            );
            (lines, None)
        }
        None => {
            let Some(Ok(listing)) = &files.listing else {
                return (lines, None);
            };
            if listing.entries.is_empty() {
                lines.push(Line::styled("empty", muted));
            }
            for (at, entry) in listing.entries.iter().enumerate() {
                let on = at == files.index;
                let said = format!("{}{}", mark(on), entry.label(&files.path));
                lines.push(Line::styled(cut(&said, width), picked(on)));
            }
            if listing.truncated {
                lines.push(Line::styled("… there are more", muted));
            }
            (lines, Some(head + files.index))
        }
    }
}

fn changes_lines(chat: &Chat, width: usize) -> Vec<Line<'static>> {
    let p = palette();
    let muted = Style::default().fg(p.text_muted);
    let mut lines = Vec::new();
    match workspace_trouble(chat, &chat.side.summary) {
        Some(trouble) => {
            lines.extend(
                wrap(&trouble, width)
                    .into_iter()
                    .map(|l| Line::styled(l, muted)),
            );
            return lines;
        }
        None => {
            if let Some(Ok(summary)) = &chat.side.summary {
                lines.push(heading(&cut(&git_line(summary), width), p));
                for change in &summary.git.changes {
                    lines.push(Line::styled(
                        cut(&clean(change), width),
                        Style::default().fg(p.text_secondary),
                    ));
                }
            }
        }
    }
    lines.push(Line::styled(
        "─".repeat(width),
        Style::default().fg(p.border),
    ));
    match &chat.side.diff {
        None => lines.push(Line::styled("reading…", muted)),
        Some(Err(failure)) => lines.push(Line::styled(describe(failure), muted)),
        Some(Ok(diff)) if diff.text.trim().is_empty() => {
            lines.push(Line::styled("no changes to tracked files", muted))
        }
        Some(Ok(diff)) => {
            for line in clean(&diff.text).lines() {
                let style = match line.chars().next() {
                    Some('+') if !line.starts_with("+++") => Style::default().fg(p.success),
                    Some('-') if !line.starts_with("---") => Style::default().fg(p.danger),
                    Some('@') => Style::default().fg(p.info),
                    Some('d' | 'i' | '+' | '-') => {
                        Style::default().fg(p.text).add_modifier(Modifier::BOLD)
                    }
                    _ => Style::default().fg(p.text_secondary),
                };
                lines.push(Line::styled(cut(line, width), style));
            }
            if diff.truncated {
                lines.push(Line::styled("… the rest was left out", muted));
            }
        }
    }
    lines
}

fn tree_lines(
    tree: &ExecutionTree,
    lead: &str,
    child_lead: &str,
    width: usize,
    lines: &mut Vec<Line<'static>>,
) {
    let p = palette();
    let execution = &tree.execution;
    let mut said = format!(" {}", execution.workflow_id);
    if let Some(caller) = &execution.caller {
        said.push_str(&format!(" by {caller}"));
    }
    if let Some(tag) = &execution.tag {
        said.push_str(&format!(" [{tag}]"));
    }
    let state = execution_state(&execution.state, p);
    let head = format!("{lead}{} ", execution.id);
    let room = width.saturating_sub(head.chars().count() + state.width());
    lines.push(Line::from(vec![
        Span::styled(head, Style::default().fg(p.text_secondary)),
        state,
        Span::styled(cut(&said, room), Style::default().fg(p.text_muted)),
    ]));
    let count = tree.children.len();
    for (at, child) in tree.children.iter().enumerate() {
        let (branch, rest) = if at + 1 == count {
            ("└─ ", "   ")
        } else {
            ("├─ ", "│  ")
        };
        tree_lines(
            child,
            &format!("{child_lead}{branch}"),
            &format!("{child_lead}{rest}"),
            width,
            lines,
        );
    }
}

fn workers_lines(app: &App, chat: &Chat, width: usize) -> Vec<Line<'static>> {
    let p = palette();
    let muted = Style::default().fg(p.text_muted);
    let mut lines = Vec::new();
    match &chat.side.tree {
        None => lines.push(Line::styled("reading…", muted)),
        Some(Err(failure)) => lines.push(Line::styled(describe(failure), muted)),
        Some(Ok(tree)) => {
            tree_lines(tree, "", "", width, &mut lines);
            lines.push(Line::raw(""));
            let said = if tree.children.is_empty() {
                "No execution was started by this one: nothing was delegated.".to_string()
            } else {
                format!(
                    "Each one is an execution of its own: {} attaches to one that has an agent.",
                    app.options.keys.code_sessions.label()
                )
            };
            lines.extend(
                wrap(&said, width)
                    .into_iter()
                    .map(|l| Line::styled(l, muted)),
            );
        }
    }
    lines
}

fn draw_side(frame: &mut Frame, app: &App, chat: &Chat, area: Rect, cache: &mut Cache) {
    let p = palette();
    let focused = chat.focus == Focus::Side && chat.asking.is_none();
    let block = pane("Session", focused, p);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 2 || inner.width < 8 {
        return;
    }
    // A column free at each side.
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width - 2,
        ..inner
    };
    let [tabs, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(inner);
    let mut spans = Vec::new();
    for tab in Tab::ALL {
        let style = if tab == chat.side.tab {
            Style::default()
                .fg(p.focus)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(p.text_muted)
        };
        spans.push(Span::styled(tab.title(), style));
        spans.push(Span::raw("  "));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), tabs);
    let width = body.width as usize;
    let height = body.height as usize;
    let (lines, cursor) = match chat.side.tab {
        Tab::Activity => (activity_lines(app, chat, &cache.turns, width), None),
        Tab::Files => files_lines(chat, width),
        Tab::Changes => (changes_lines(chat, width), None),
        Tab::Workers => (workers_lines(app, chat, width), None),
    };
    let room = lines.len().saturating_sub(height);
    cache.side_room = room;
    // A list keeps its cursor in sight; a text starts where it was scrolled to.
    let top = match (cursor, chat.side.tab, &chat.side.files.viewer) {
        (Some(row), ..) => (row + 1).saturating_sub(height),
        (None, Tab::Files, Some(viewer)) => viewer.scroll.min(room),
        (None, Tab::Files, None) => 0,
        (None, ..) => chat.side.scroll.min(room),
    };
    let shown: Vec<Line<'static>> = lines.into_iter().skip(top).take(height).collect();
    frame.render_widget(Paragraph::new(shown), body);
}

// ── Setup, footer and overlays ────────────────────────────────────────────────────────

fn draw_setup(frame: &mut Frame, app: &App, setup: &Setup, area: Rect) {
    let p = palette();
    let muted = Style::default().fg(p.text_muted);
    let width = area.width.saturating_sub(4).min(BOX_WIDTH);
    let inner = width.saturating_sub(4) as usize;
    let form = &setup.form;
    let mut lines: Vec<Line<'static>> = Vec::new();
    if app.settings.mascot {
        let beat = mascot::beat(app.now, app.options.tuning.code_twinkle_ms);
        let facts = [
            Line::styled(
                format!("Set up {}", app.settings.label),
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                cut(
                    &format!("template {}", form.template),
                    inner.saturating_sub(11),
                ),
                muted,
            ),
            Line::styled(
                cut(&clean(&setup.description), inner.saturating_sub(11)),
                muted,
            ),
        ];
        for (face, fact) in mascot::lines(app.mood(), beat, p).into_iter().zip(facts) {
            let mut spans = face.spans;
            spans.push(Span::raw("   "));
            spans.extend(fact.spans);
            lines.push(Line::from(spans));
        }
    } else {
        lines.push(heading(&format!("Set up {}", app.settings.label), p));
        lines.push(Line::styled(cut(&clean(&setup.description), inner), muted));
    }
    lines.push(Line::raw(""));
    let name_width = form
        .vars
        .iter()
        .map(|var| var.name.chars().count() + 1)
        .max()
        .unwrap_or(0)
        .max("profile id".len());
    let mut cursor_row = None;
    let mut cursor_column = 0;
    for (at, row) in form.rows().into_iter().enumerate() {
        let on = at == form.row;
        let mark = if on { "▸ " } else { "  " };
        let (name, value, hint) = match row {
            FormRow::Var(i) => {
                let var = &form.vars[i];
                let star = if var.default.is_none() { "*" } else { "" };
                (
                    format!("{}{star}", var.name),
                    form.values[i].clone(),
                    var.description.clone(),
                )
            }
            FormRow::Id => (
                "profile id".to_string(),
                form.id.clone(),
                "the profile this is written to".to_string(),
            ),
            FormRow::Force => (
                "replace".to_string(),
                if form.force { "[x]" } else { "[ ]" }.to_string(),
                "write over the profile if it is there (what was edited in it is lost)".to_string(),
            ),
        };
        let room = inner.saturating_sub(name_width + 4).max(1);
        // The end of a long value stays in sight: that is where typing goes.
        let chars: Vec<char> = clean(&value).chars().collect();
        let tail: String = chars[chars.len().saturating_sub(room)..].iter().collect();
        if on {
            cursor_row = Some(lines.len());
            cursor_column = 2 + name_width + 1 + tail.chars().count();
        }
        let style = if on {
            Style::default().fg(p.text)
        } else {
            Style::default().fg(p.text_secondary)
        };
        lines.push(Line::from(vec![
            Span::styled(mark, Style::default().fg(p.focus)),
            Span::styled(format!("{name:<name_width$} "), muted),
            Span::styled(tail, style),
        ]));
        if on && !hint.is_empty() {
            lines.extend(
                wrap(&clean(&hint), inner.saturating_sub(name_width + 3))
                    .into_iter()
                    .map(|l| Line::styled(format!("  {:name_width$} {l}", ""), muted)),
            );
        }
    }
    if !setup.requires.is_empty() {
        lines.push(Line::raw(""));
        lines.push(heading("Needs", p));
        for (require, state) in &setup.requires {
            let (symbol, word) = state.label();
            let color = match word {
                "installed" => p.success,
                "broken" => p.danger,
                _ => p.text_muted,
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {symbol} {word} "), Style::default().fg(color)),
                Span::styled(
                    cut(
                        &format!("{} {}", require.module, require.version),
                        inner.saturating_sub(18),
                    ),
                    muted,
                ),
            ]));
        }
    }
    lines.push(Line::raw(""));
    if let Some(error) = &setup.error {
        for line in clean(error).lines() {
            lines.extend(
                wrap(line, inner)
                    .into_iter()
                    .map(|l| Line::styled(l, Style::default().fg(p.danger))),
            );
        }
        lines.push(Line::raw(""));
    }
    let notes = [
        "Commands the agent runs start in this folder. They are not sandboxed.",
        "A new session starts the model without the context of earlier ones.",
    ];
    for note in notes {
        lines.extend(
            wrap(note, inner)
                .into_iter()
                .map(|l| Line::styled(l, muted)),
        );
    }
    let height = (lines.len() as u16 + 2).min(area.height);
    let place = centered(area, width, height);
    let block = pane("Setup", true, p);
    let text = block.inner(place);
    frame.render_widget(block, place);
    // A column free at each side.
    let text = Rect {
        x: text.x + 1,
        width: text.width.saturating_sub(2),
        ..text
    };
    frame.render_widget(Paragraph::new(lines), text);
    if let Some(row) = cursor_row {
        let x = text.x + cursor_column as u16;
        let y = text.y + row as u16;
        if x < text.right() && y < text.bottom() && app.overlay.is_none() {
            frame.set_cursor_position((x, y));
        }
    }
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let p = palette();
    let k = &app.options.keys;
    let keys = match &app.stage {
        Stage::Chat(chat) if chat.asking.is_some() => format!(
            "a permission request has the keys · {} puts it off · {} cancels the turn",
            k.back.label(),
            k.cancel_turn.label()
        ),
        Stage::Chat(chat) if chat.focus == Focus::Side => format!(
            "{}{} tab · ↑↓ move · {} open · {} up · {} search · {} read again · {} back",
            k.code_previous_tab.label(),
            k.code_next_tab.label(),
            k.open.label(),
            k.code_parent.label(),
            k.code_search.label(),
            k.code_refresh.label(),
            k.back.label()
        ),
        Stage::Chat(_) => format!(
            "{} side panel · {}/{} scroll · {} cancel turn · {} sessions · {} keys · {} leave",
            k.code_focus.label(),
            k.code_page_up.label(),
            k.code_page_down.label(),
            k.cancel_turn.label(),
            k.code_sessions.label(),
            k.code_help.label(),
            k.code_quit.label()
        ),
        Stage::Setup(_) => format!(
            "↑↓ field · type to change it · Space ticks replace · {} launch · {} leave",
            k.open.label(),
            k.back.label()
        ),
        Stage::Blocked(_) => format!(
            "{} try again · {} leave",
            k.code_refresh.label(),
            k.back.label()
        ),
        Stage::Opening | Stage::Launching { .. } => format!("{} leave", k.code_quit.label()),
    };
    let width = area.width.saturating_sub(1) as usize;
    let line = match &app.notice {
        Some((Tone::Error, text)) if text.starts_with(['✗', '□']) => Line::styled(
            format!(" {}", cut(text, width)),
            Style::default().fg(p.danger),
        ),
        Some((Tone::Error, text)) => Line::styled(
            format!(" ✗ {}", cut(text, width.saturating_sub(2))),
            Style::default().fg(p.danger),
        ),
        Some((Tone::Info, text)) => Line::styled(
            format!(" {}", cut(text, width)),
            Style::default().fg(p.info),
        ),
        None => Line::styled(
            format!(" {}", cut(&keys, width)),
            Style::default().fg(p.text_muted),
        ),
    };
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(p.sidebar)),
        area,
    );
}

fn draw_confirm(frame: &mut Frame, title: &str, said: &str, keys: &str) {
    let p = palette();
    let area = centered(frame.area(), 64, 6);
    let width = area.width.saturating_sub(4) as usize;
    frame.render_widget(Clear, area);
    let mut lines: Vec<Line<'static>> = wrap(said, width).into_iter().map(Line::raw).collect();
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        cut(keys, width),
        Style::default().fg(p.text_muted),
    ));
    frame.render_widget(Paragraph::new(lines).block(pane(title, true, p)), area);
}

/// Every key and command, with the keys as they are bound now.
fn draw_help(frame: &mut Frame, app: &App) {
    let p = palette();
    let k = &app.options.keys;
    let muted = Style::default().fg(p.text_muted);
    let row = |keys: String, what: &str| {
        Line::from(vec![
            Span::styled(format!(" {keys:<22}"), Style::default().fg(p.focus)),
            Span::styled(what.to_string(), Style::default().fg(p.text_secondary)),
        ])
    };
    let scroll = format!("{} / {}", k.code_page_up.label(), k.code_page_down.label());
    let tabs = format!(
        "{} / {}",
        k.code_previous_tab.label(),
        k.code_next_tab.label()
    );
    let mut lines = vec![
        heading("Typing", p),
        row(k.code_send.label(), "send what is typed"),
        row(k.code_newline.label(), "a new line"),
        row(
            "↑ / ↓".into(),
            "move between lines; past the ends, earlier prompts",
        ),
        row(k.cancel_turn.label(), "cancel the turn in flight"),
        row(scroll, "scroll the conversation"),
        row(k.back.label(), "back to the newest"),
        heading("Session", p),
        row(
            k.code_permission.label(),
            "answer the oldest permission request",
        ),
        row(
            k.code_sessions.label(),
            "attach to another session, or start a new one",
        ),
        row(k.code_stop.label(), "stop the execution (asks first)"),
        row(
            k.code_quit.label(),
            "clear what is typed; with nothing typed, leave",
        ),
        heading("Side panel", p),
        row(k.code_side.label(), "show or hide it"),
        row(k.code_focus.label(), "give it the keys, and take them back"),
        row(tabs, "its tabs: activity, files, changes, workers"),
        row(k.code_parent.label(), "the folder above, or out of a file"),
        row(k.code_search.label(), "search the workspace"),
        row(k.code_refresh.label(), "read it again"),
        heading(
            &format!(
                "Commands · type / for a menu: ↑ / ↓ pick, {} complete, {} close",
                k.code_complete.label(),
                k.back.label()
            ),
            p,
        ),
    ];
    let said = "The harness behind this screen is a profile: open it in the canvas of the \
                app, or in `regulus`, to change its model, tools and rules.";
    let width = BOX_WIDTH.min(frame.area().width.saturating_sub(2));
    let note: Vec<Line<'static>> = wrap(said, width.saturating_sub(4) as usize)
        .into_iter()
        .map(|l| Line::styled(format!(" {l}"), muted))
        .collect();
    // The registry's commands (`commands.rs`): what is listed here is what runs and what the
    // menu suggests. One row each with what it does when the terminal is tall enough for
    // them; otherwise their usages run together, and the menu says what each one does.
    let tail = 2 + note.len() + 2;
    if lines.len() + COMMANDS.len() + tail <= frame.area().height as usize {
        for spec in COMMANDS {
            let mut said = spec.description.to_string();
            if !spec.aliases.is_empty() {
                let also: Vec<String> = spec.aliases.iter().map(|a| format!("/{a}")).collect();
                said = format!("{said} · {}", also.join(" "));
            }
            lines.push(row(spec.usage(), &said));
        }
    } else {
        let usages: Vec<String> = COMMANDS.iter().map(|spec| spec.usage()).collect();
        let focus = Style::default().fg(p.focus);
        lines.extend(
            wrap(&usages.join("  "), width.saturating_sub(4) as usize)
                .into_iter()
                .map(|l| Line::styled(format!(" {l}"), focus)),
        );
    }
    lines.push(row("//text".into(), "send a message that starts with /"));
    lines.push(Line::raw(""));
    lines.extend(note);
    let area = centered(frame.area(), width, lines.len() as u16 + 2);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines).block(pane("Keys and commands · any key closes", true, p)),
        area,
    );
}

fn draw_sessions(frame: &mut Frame, app: &App, index: usize) {
    let p = palette();
    let muted = Style::default().fg(p.text_muted);
    let rows = app.pick_rows();
    let width = BOX_WIDTH.min(frame.area().width.saturating_sub(2));
    let inner = width.saturating_sub(4) as usize;
    let current = app.chat().map(|chat| chat.session.execution.id.as_str());
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        let on = at == index;
        let mark = if on { "▸ " } else { "  " };
        let style = if on {
            Style::default().fg(p.text).bg(p.selected)
        } else {
            Style::default().fg(p.text_secondary)
        };
        match row {
            PickRow::New(profile) => lines.push(Line::styled(
                cut(&format!("{mark}+ a new session of {profile}"), inner),
                style,
            )),
            PickRow::Session(candidate) => {
                let execution = &candidate.execution;
                let here = if current == Some(execution.id.as_str()) {
                    " (this one)"
                } else {
                    ""
                };
                let mut said = format!(" {}/{}{here}", execution.profile_id, execution.workflow_id);
                if let Some(parent) = &execution.parent {
                    said.push_str(&format!(" · worker of {parent}"));
                }
                if let Some(folder) = &candidate.folder {
                    said.push_str(&format!(" · {folder}"));
                }
                let state = execution_state(&execution.state, p);
                let head = format!("{mark}{} ", execution.id);
                let room = inner.saturating_sub(head.chars().count() + state.width());
                lines.push(Line::from(vec![
                    Span::styled(head, style),
                    state,
                    Span::styled(cut(&said, room), muted),
                ]));
            }
        }
    }
    if app.survey.is_none() {
        lines.push(Line::styled("reading…", muted));
    } else if rows.iter().all(|row| matches!(row, PickRow::New(_))) {
        lines.push(Line::styled("no execution has an agent to talk to", muted));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled(
        format!(
            "↑↓ choose · {} attach · {} back",
            app.options.keys.open.label(),
            app.options.keys.back.label()
        ),
        muted,
    ));
    let height = (lines.len() as u16 + 2).min(frame.area().height);
    // The cursor stays in sight in a long list.
    let room = height.saturating_sub(2) as usize;
    let top = (index + 1).saturating_sub(room.saturating_sub(2).max(1));
    let shown: Vec<Line<'static>> = if lines.len() > room {
        lines.into_iter().skip(top).take(room).collect()
    } else {
        lines
    };
    let area = centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(shown).block(pane("Sessions", true, p)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::commands;
    use crate::code::fixtures::*;
    use crate::code::inspect::Ask;
    use crate::code::launch::Survey;
    use crate::code::link::Event;
    use crate::rpc::CallError;
    use crate::tui::core;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyCode;
    use ratatui::Terminal;
    use regulus_proto::code;
    use serde_json::json;

    /// What the screen shows at a size, row by row.
    fn screen_at(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut cache = Cache::default();
        terminal.draw(|frame| draw(frame, app, &mut cache)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn screen(app: &App) -> String {
        screen_at(app, 140, 40)
    }

    fn show_side(app: &mut App) {
        if let Stage::Chat(chat) = &mut app.stage {
            chat.side.shown = true;
        }
    }

    fn has(shown: &str, text: &str) {
        assert!(shown.contains(text), "no `{text}` in:\n{shown}");
    }

    fn has_not(shown: &str, text: &str) {
        assert!(!shown.contains(text), "`{text}` in:\n{shown}");
    }

    #[test]
    fn status_row_uses_only_selected_fields_and_can_be_hidden() {
        let mut app = chatting();
        app.settings.status_fields = vec![StatusField::Session];
        let shown = screen(&app);
        has(&shown, "session exec-1");
        has_not(&shown, "root /work/app");
        app.settings.status_bar = false;
        let hidden = screen(&app);
        has_not(&hidden, "session exec-1");
        has(&hidden, "Enter send · Alt+Enter new line");
    }

    /// A turn that read a file, asked to write it and waits.
    fn asking() -> App {
        let mut app = chatting();
        app.apply(history(
            0,
            4,
            json!([entry(1, "user", "edit the main file", "t-1")]),
            json!([
                activity(
                    2,
                    "t-1",
                    json!({ "kind": "tool_call", "id": "c1",
                    "tool_kind": "read", "title": "code_read_file src/main.rs",
                    "status": "completed" })
                ),
                activity(
                    3,
                    "t-1",
                    json!({ "kind": "plan", "entries": [
                    { "content": "read the code", "status": "completed" },
                    { "content": "change it", "status": "in_progress" },
                    { "content": "run the tests", "status": "pending" }] })
                ),
                asks(4, "p1")
            ]),
            "busy",
        ));
        app
    }

    #[test]
    fn the_setup_form_says_what_it_writes_and_what_holds() {
        let mut app = app_for(wanted(), false);
        app.apply(Event::Surveyed(Ok(Box::new(survey()))));
        let shown = screen(&app);
        has(&shown, "Set up Regulus Code");
        has(&shown, " \\\\|// ");
        has(&shown, "={o*o}=");
        has(&shown, "template regulus-code");
        has(&shown, "▸ root*       /work/app");
        has(&shown, "The folder");
        has(&shown, "model       qwen3:4b");
        has(&shown, "profile id  code-app-");
        has(&shown, "replace     [ ]");
        has(&shown, "□ not installed regulus/code >=0.1");
        has(&shown, "They are not sandboxed.");
        has(&shown, "without the context of earlier ones");
        has(&shown, "Enter launch · Esc leave");
        // What the core refused is on the form.
        app.apply(Event::Surveyed(Ok(Box::new(survey()))));
        press(&mut app, KeyCode::Enter);
        app.apply(Event::Launched(Err(CallError {
            code: Some(code::TEMPLATE_UNMET),
            message: "regulus/code is not installed".into(),
        })));
        has(
            &screen(&app),
            "a required module is missing: regulus/code is not installed",
        );
    }

    #[test]
    fn opening_and_being_stopped_say_what_goes_on() {
        let mut app = app_for(wanted(), false);
        let shown = screen(&app);
        has(&shown, "{*} REGULUS CODE");
        has(&shown, "connecting to the core");
        has(&shown, "… connecting");
        app.apply(Event::Core(core::Event::Connection(
            core::Connection::Starting,
        )));
        has(&screen(&app), "starting the core");
        app.apply(Event::Core(core::Event::Connection(
            core::Connection::Failed("no core has run in /h; start one with: regulus serve".into()),
        )));
        let shown = screen(&app);
        has(&shown, "✗ no core");
        has(&shown, "start one with: regulus serve");
        has(&shown, "={x*x}=");
        app.apply(Event::Surveyed(Err(CallError {
            code: None,
            message: "the core does not answer".into(),
        })));
        let shown = screen(&app);
        has(&shown, "cannot go on");
        has(&shown, "r try again · Esc leave");
        // A step under way says which.
        let mut app = app_for(wanted(), true);
        app.apply(Event::Surveyed(Ok(Box::new(survey()))));
        has(&screen(&app), "writing code-app-");
    }

    #[test]
    fn an_empty_conversation_welcomes_with_who_where_and_what_holds() {
        let mut app = chatting();
        show_side(&mut app);
        app.apply(inspected(
            Ask::summary(),
            json!({ "root": "/work/app", "tools": ["code_read_file"],
                    "git": { "branch": "main", "status": " M a.rs\n" } }),
        ));
        let shown = screen(&app);
        has(&shown, "{*} REGULUS CODE");
        has(&shown, "root /work/app");
        has(&shown, "branch ⎇ main · 1 change");
        has(&shown, "state ✓ ready");
        has(&shown, "session exec-1");
        has(&shown, "={o*o}=   /work/app");
        has(&shown, "Say what you want done and press Enter.");
        has(&shown, "They are not sandboxed.");
        has(&shown, "Regulus' record of the conversation");
        has(&shown, "│ › what should the agent do?");
        has(&shown, "Enter send · Alt+Enter new line");
        has(&shown, "Activity  Files  Changes  Workers");
        has(&shown, "@agent · ✓ ready");
        has(&shown, "tools: code_read_file");
        has(&shown, "attached to exec-1");
        // The footer says what happened until a key is pressed, then the keys.
        app.notice = None;
        has(&screen(&app), "Ctrl+C leave");
        // The label and the mascot are settings.
        app.settings.label = "Leo".into();
        app.settings.mascot = false;
        let shown = screen(&app);
        has(&shown, "LEO");
        has_not(&shown, "{*}");
        has_not(&shown, "={o*o}=");
    }

    #[test]
    fn a_turn_is_drawn_as_cards_and_a_request_opens_its_bar() {
        let mut app = asking();
        show_side(&mut app);
        app.notice = None;
        let shown = screen(&app);
        has(&shown, "› edit the main file");
        has(&shown, "✓ code_read_file src/main.rs (read) · completed");
        has(&shown, "◇ plan");
        has(&shown, "… change it");
        has(
            &shown,
            "? permission: Write src/main.rs (code_write_file) · waiting for you (Ctrl+P)",
        );
        has(&shown, "… working");
        has(&shown, "? Permission");
        has(&shown, "│ ▸ 1 Allow once (allow_once)");
        has(&shown, "│   3 Reject (reject_once)");
        has(
            &shown,
            "1-3 pick · y allow · n reject · Enter answer · Esc later",
        );
        has(&shown, "state ? waiting for you");
        has(&shown, "a permission request has the keys");
        // The side panel says the same in its own words.
        has(&shown, "Plan");
        has(&shown, "Tools of this turn");
        has(&shown, "Waiting for you");
        has(&shown, "? Write src/main.rs · Ctrl+P");
        // Answered: the card says with what, the bar goes, the answer is the text.
        app.apply(history(
            4,
            8,
            json!([entry(
                8,
                "agent",
                "Done:\n```rust\n    let x = 1;\n```",
                "t-1"
            )]),
            json!([
                activity(
                    5,
                    "t-1",
                    json!({ "kind": "permission_resolved", "id": "p1",
                    "option": "always" })
                ),
                activity(
                    6,
                    "t-1",
                    json!({ "kind": "tool_call", "id": "c2",
                    "title": "code_write_file src/main.rs", "status": "pending" })
                ),
                activity(
                    7,
                    "t-1",
                    json!({ "kind": "tool_update", "id": "c2",
                    "status": "completed",
                    "content": "one\ntwo\nthree\nfour\nfive\nsix" })
                )
            ]),
            "idle",
        ));
        let shown = screen(&app);
        has(
            &shown,
            "✓ permission: Write src/main.rs (code_write_file) → Always allow",
        );
        has(&shown, "✓ code_write_file src/main.rs · completed");
        has(&shown, "│ four");
        has(&shown, "│ … 2 more lines");
        has(&shown, "✦ Done:");
        // Code keeps the spaces it starts with.
        has(&shown, "      let x = 1;");
        has_not(&shown, "? Permission");
        has_not(&shown, "… working");
        has(&shown, "state ✓ ready");
        // What was sent and not recorded yet, and what the box says went wrong.
        say(&mut app, "and now the tests");
        app.apply(history(
            8,
            9,
            json!([entry(9, "system", "the engine went away", "t-2")]),
            json!([]),
            "idle",
        ));
        let shown = screen(&app);
        has(&shown, "› and now the tests");
        has(&shown, "… queued");
        has(&shown, "! the engine went away");
    }

    #[test]
    fn the_side_panel_shows_files_changes_and_workers() {
        let mut app = chatting();
        say(&mut app, "/files");
        app.apply(inspected(
            Ask::list(""),
            json!({ "path": "", "truncated": true, "entries": [
                { "path": "notes.md", "kind": "file" }, { "path": "src", "kind": "directory" }] }),
        ));
        let shown = screen(&app);
        has(&shown, "▸ src/");
        has(&shown, "  notes.md");
        has(&shown, "… there are more");
        say(&mut app, "/read src/main.rs");
        app.apply(inspected(
            Ask::read("src/main.rs"),
            json!({ "path": "src/main.rs", "text": "fn main() {\n\tprintln!();\n}\n",
                    "truncated": true }),
        ));
        let shown = screen(&app);
        has(&shown, "src/main.rs");
        has(&shown, "1 fn main() {");
        has(&shown, "2     println!();");
        has(&shown, "… the rest was left out");
        say(&mut app, "/files");
        say(&mut app, "/search needle");
        has(&screen(&app), "reading…");
        app.apply(inspected(
            Ask::search("needle"),
            json!({ "matches": [{ "path": "notes.md", "line": 1, "text": "a needle\n" }] }),
        ));
        let shown = screen(&app);
        has(&shown, "“needle”");
        has(&shown, "▸ notes.md:1 a needle");
        say(&mut app, "/diff");
        app.apply(inspected(
            Ask::summary(),
            json!({ "root": "/work/app", "git": { "branch": "main",
                    "status": " M src/main.rs\n?? notes.md\n" } }),
        ));
        app.apply(inspected(
            Ask::diff(),
            json!({ "text": "diff --git a/src/main.rs b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n" }),
        ));
        let shown = screen(&app);
        has(&shown, "⎇ main · 2 changes");
        has(&shown, " M src/main.rs");
        has(&shown, "diff --git a/src/main.rs");
        has(&shown, "+new");
        say(&mut app, "/workers");
        let tree = json!({ "execution": execution(EXEC, "code-app", "running"), "children": [
            { "execution": { "id": "exec-2", "profile_id": "code-app", "workflow_id": "worker",
                "project_id": null, "state": { "status": "running" }, "parent": EXEC,
                "caller": "workers", "tag": "w-1" }, "children": [] }] });
        app.apply(Event::Tree {
            execution: EXEC.into(),
            result: Ok(serde_json::from_value(tree).unwrap()),
        });
        let shown = screen(&app);
        has(&shown, "exec-1 ✓ running code");
        has(&shown, "└─ exec-2 ✓ running worker by workers");
        has(&shown, "Ctrl+O attaches to one that has an agent");
    }

    #[test]
    fn what_the_workspace_cannot_say_is_said_as_what_it_is() {
        // No module serves `code.inspect`: not installed, never a generic error.
        let mut app = chatting();
        show_side(&mut app);
        let missing = |code: i32, message: &str| Event::Inspected {
            execution: EXEC.into(),
            node: "workspace".into(),
            ask: Ask::summary(),
            result: Err(CallError {
                code: Some(code),
                message: message.into(),
            }),
        };
        app.apply(missing(core::METHOD_NOT_FOUND, "method not found"));
        has(&screen(&app), "□ not installed");
        app.apply(missing(code::MODULE_UNAVAILABLE, "the module is broken"));
        has(&screen(&app), "✗ unavailable: the module is broken");
        // Not a repository: git has nothing to say, and that is all.
        app.apply(inspected(
            Ask::summary(),
            json!({ "root": "/work/app", "git": null }),
        ));
        let shown = screen(&app);
        has(&shown, "branch git: not a git repository");
        has(&shown, "REGULUS CODE");
        // A workflow with an agent and no workspace: the conversation works, the rest says why.
        let mut alone = session(EXEC);
        alone.workspace = None;
        let mut app = app_for(wanted(), false);
        let asked_for = app.apply(Event::Launched(Ok(Box::new(alone))));
        show_side(&mut app);
        assert!(asked(&asked_for).is_empty());
        has(&screen(&app), "no workspace node in this workflow");
        say(&mut app, "/files");
        has(&screen(&app), "no workspace node in this workflow");
        // The conversation itself cannot be read.
        app.apply(Event::Core(core::Event::AgentHistory {
            execution: EXEC.into(),
            node: "agent".into(),
            since: 0,
            result: Err(CallError {
                code: Some(core::METHOD_NOT_FOUND),
                message: "method not found".into(),
            }),
        }));
        has(
            &screen(&app),
            "the conversation cannot be read: □ not installed",
        );
    }

    /// The row of the screen a text first shows on.
    fn row_of(shown: &str, text: &str) -> usize {
        shown
            .lines()
            .position(|line| line.contains(text))
            .unwrap_or_else(|| panic!("no `{text}` in:\n{shown}"))
    }

    #[test]
    fn a_slash_shows_the_commands_above_the_prompt_and_follows_what_is_typed() {
        let mut app = chatting();
        typed(&mut app, "/");
        let shown = screen(&app);
        has(&shown, "Commands · ↑↓ pick · Tab complete · Esc close");
        has(&shown, "list a folder of the workspace · /ls");
        has(&shown, "▸ /files [folder]");
        // Compact: a few rows of the list, and where the chosen one is in it.
        has(&shown, &format!("1/{}", commands::COMMANDS.len()));
        assert!(row_of(&shown, "Commands ·") < row_of(&shown, "› /"));
        assert!(row_of(&shown, "/files [folder]") < row_of(&shown, "› /"));
        has_not(&shown, "/quit");
        // The chosen row moves, and the window with it.
        press(&mut app, KeyCode::Down);
        has(&screen(&app), "▸ /read <file>");
        for _ in 0..20 {
            press(&mut app, KeyCode::Down);
        }
        let shown = screen(&app);
        has(&shown, "▸ /quit");
        has(&shown, &format!("{0}/{0}", commands::COMMANDS.len()));
        has_not(&shown, "/files [folder]");
        // Filtered by what follows: one command, with its aliases.
        let mut app = chatting();
        typed(&mut app, "/di");
        let shown = screen(&app);
        has(&shown, "▸ /diff");
        has(&shown, "what changed · /changes");
        has_not(&shown, "/files [folder]");
        has_not(&shown, "/1");
    }

    #[test]
    fn no_match_arguments_and_dismissal_draw_no_menu() {
        for text in ["/zzz", "/files src", "//files", "hello", ""] {
            let mut app = chatting();
            typed(&mut app, text);
            has_not(&screen(&app), "Commands ·");
        }
        let mut app = chatting();
        typed(&mut app, "/di");
        has(&screen(&app), "Commands ·");
        press(&mut app, KeyCode::Esc);
        has_not(&screen(&app), "Commands ·");
        has(&screen(&app), "› /di");
        typed(&mut app, "f");
        has(&screen(&app), "Commands ·");
    }

    #[test]
    fn a_pasted_command_shows_its_menu_and_is_not_run() {
        let mut app = chatting();
        app.paste("/stop");
        let shown = screen(&app);
        has(&shown, "▸ /stop");
        has(&shown, "› /stop");
        has_not(&shown, "Stop exec-1?");
        assert_eq!(app.overlay, None);
    }

    #[test]
    fn the_menu_gives_way_to_what_has_the_keys_and_to_small_screens() {
        // The panel, a permission request and an overlay each have the keys: no menu.
        let mut app = chatting();
        typed(&mut app, "/");
        has(&screen(&app), "Commands ·");
        let focus = |app: &mut App, to: Focus| {
            if let Stage::Chat(chat) = &mut app.stage {
                chat.focus = to;
            }
        };
        focus(&mut app, Focus::Side);
        has_not(&screen(&app), "Commands ·");
        focus(&mut app, Focus::Composer);
        app.tick(1000);
        app.apply(history(0, 2, json!([]), json!([asks(2, "p1")]), "busy"));
        ctrl(&mut app, 'p');
        let shown = screen(&app);
        has(&shown, "? Permission");
        has_not(&shown, "Commands ·");
        press(&mut app, KeyCode::Esc);
        has(&screen(&app), "Commands ·");
        app.overlay = Some(Overlay::Help);
        has_not(&screen(&app), "Commands · ↑↓");
        app.overlay = None;
        // Where there is room for one row it is one; where there is none, nothing and no panic.
        let shown = screen_at(&app, 60, 9);
        has(&shown, "Commands ·");
        has(&shown, "› /");
        for height in 0..9 {
            screen_at(&app, 30, height);
        }
        // The rows are the composer's own limit.
        app.options.tuning.code_composer_rows = 2;
        let shown = screen(&app);
        has(&shown, "▸ /files [folder]");
        has(&shown, "/read <file>");
        has_not(&shown, "/search <text>");
    }

    #[test]
    fn the_menu_names_the_completion_key_as_bound() {
        let table: toml::Table =
            toml::from_str(r#"keys = { code_complete = "ctrl+g", back = "ctrl+q" }"#).unwrap();
        let mut app = chatting();
        app.options = crate::tui::keys::Options::from_client(Some(&table)).unwrap();
        typed(&mut app, "/di");
        has(&screen(&app), "Ctrl+G complete · Ctrl+Q close");
        ctrl(&mut app, 'g');
        assert_eq!(chat(&app).composer.text(), "/diff");
        // Tab is the side panel's again.
        press(&mut app, KeyCode::Tab);
        assert_eq!(chat(&app).focus, Focus::Side);
    }

    #[test]
    fn the_help_lists_the_registry_and_fits_the_terminal() {
        let mut app = chatting();
        app.overlay = Some(Overlay::Help);
        // Tall: a row for each, with what it does and its aliases.
        let tall = screen_at(&app, 140, 50);
        for spec in commands::COMMANDS {
            has(&tall, &format!("{:<22}{}", spec.usage(), spec.description));
        }
        has(&tall, "list a folder of the workspace · /ls");
        has(&tall, "leave · /exit");
        has(&tall, "open it in the canvas");
        // Short: every usage still, the menu says the rest, and the note is not cut.
        let short = screen_at(&app, 140, 40);
        for spec in commands::COMMANDS {
            has(&short, &spec.usage());
        }
        has(&short, "type / for a menu");
        has(&short, "//text");
        has(&short, "open it in the canvas");
        has_not(&short, "list a folder of the workspace");
    }

    #[test]
    fn overlays_list_the_keys_as_bound_and_the_sessions_there_are() {
        let table: toml::Table =
            toml::from_str(r#"keys = { code_permission = "ctrl+y", code_help = "f2" }"#).unwrap();
        let mut app = chatting();
        app.options = crate::tui::keys::Options::from_client(Some(&table)).unwrap();
        app.key(ratatui::crossterm::event::KeyEvent::new(
            KeyCode::F(2),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        let shown = screen(&app);
        has(&shown, "Keys and commands");
        has(
            &shown,
            "Ctrl+Y                answer the oldest permission request",
        );
        has(&shown, "/search <text>");
        has(&shown, "open it in the canvas");
        press(&mut app, KeyCode::Esc);
        ctrl(&mut app, 'o');
        has(&screen(&app), "reading…");
        let mut worker = execution("exec-2", "code-app", "stopped");
        worker.parent = Some(EXEC.into());
        app.apply(Event::Surveyed(Ok(Box::new(Survey {
            profiles: vec![profile("code-app")],
            executions: vec![execution(EXEC, "code-app", "running"), worker],
            ..survey()
        }))));
        let shown = screen(&app);
        has(&shown, "▸ + a new session of code-app");
        has(
            &shown,
            "exec-2 ○ stopped code-app/code · worker of exec-1 · /work/app",
        );
        has(
            &shown,
            "exec-1 ✓ running code-app/code (this one) · /work/app",
        );
        press(&mut app, KeyCode::Esc);
        ctrl(&mut app, 't');
        has(
            &screen(&app),
            "Stop exec-1? Its agent and its workers end with it.",
        );
    }

    #[test]
    fn a_narrow_terminal_gives_the_conversation_the_whole_width() {
        let mut app = asking();
        show_side(&mut app);
        app.settings.status_fields = vec![StatusField::State];
        has(&screen_at(&app, 140, 40), "Activity  Files");
        let narrow = screen_at(&app, 80, 24);
        has_not(&narrow, "Activity  Files");
        has(&narrow, "› edit the main file");
        has(&narrow, "? Permission");
        // The state on the right of the header is whole; the left gives way.
        has(&narrow, "state ? waiting for you");
        // With the keys, the side panel takes the conversation's place, and gives it back.
        let mut app = chatting();
        show_side(&mut app);
        say(&mut app, "hello");
        press(&mut app, KeyCode::Tab);
        let panel = screen_at(&app, 80, 24);
        has(&panel, "Activity  Files");
        has(&panel, "@agent");
        has_not(&panel, "› hello");
        has(&panel, "│ › what should the agent do?");
        press(&mut app, KeyCode::Tab);
        let back = screen_at(&app, 80, 24);
        has(&back, "› hello");
        has_not(&back, "Activity  Files");
    }

    #[test]
    fn a_long_conversation_scrolls_and_says_there_is_more_below() {
        let mut app = chatting();
        let entries: Vec<Value> = (0..40)
            .flat_map(|n| {
                let turn = format!("t-{n}");
                [
                    entry(n * 2 + 1, "user", &format!("question {n}"), &turn),
                    entry(n * 2 + 2, "agent", &format!("answer {n}"), &turn),
                ]
            })
            .collect();
        app.apply(history(0, 80, json!(entries), json!([]), "idle"));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut cache = Cache::default();
        terminal
            .draw(|frame| draw(frame, &app, &mut cache))
            .unwrap();
        assert!(cache.transcript_room > 0);
        let newest = screen_at(&app, 100, 30);
        has(&newest, "answer 39");
        has_not(&newest, "question 0");
        press(&mut app, KeyCode::PageUp);
        let back = screen_at(&app, 100, 30);
        has(&back, "more lines · Esc goes to the newest");
        has_not(&back, "answer 39");
        say(&mut app, "/history");
        has(&screen_at(&app, 100, 30), "› question 0");
    }

    #[test]
    fn nothing_panics_however_small_the_terminal() {
        let mut setup = app_for(wanted(), false);
        setup.apply(Event::Surveyed(Ok(Box::new(survey()))));
        let mut blocked = app_for(wanted(), false);
        blocked.apply(Event::Surveyed(Err(CallError {
            code: None,
            message: "the core does not answer and this line is long enough to wrap".into(),
        })));
        let mut help = asking();
        say(&mut help, "/files");
        help.overlay = Some(Overlay::Help);
        let mut sessions = chatting();
        sessions.overlay = Some(Overlay::Sessions(0));
        let mut leaving = chatting();
        leaving.overlay = Some(Overlay::ConfirmLeave);
        let mut typing = chatting();
        typed(&mut typing, &"a very long line of text ".repeat(40));
        let apps = [
            app_for(wanted(), false),
            setup,
            blocked,
            chatting(),
            asking(),
            help,
            sessions,
            leaving,
            typing,
        ];
        for app in &apps {
            for (width, height) in [
                (1, 1),
                (2, 2),
                (8, 3),
                (20, 6),
                (31, 9),
                (40, 10),
                (79, 24),
                (111, 12),
                (300, 90),
            ] {
                let shown = screen_at(app, width, height);
                assert_eq!(shown.lines().count(), height as usize);
            }
        }
    }

    #[test]
    fn text_is_made_safe_and_wrapped_keeping_its_shape() {
        assert_eq!(clean("a\tb\x1b[2Jc\r\nd"), "a    b[2Jc\nd");
        assert_eq!(wrap("one two three", 8), vec!["one two", "three"]);
        // What does not fit goes on under the line's own indentation.
        assert_eq!(
            wrap("  - a list item that is long", 14),
            vec!["  - a list", "  item that is", "  long"]
        );
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("", 4), vec![""]);
        assert_eq!(chop("    let x = 1;", 8), vec!["    let ", "x = 1;"]);
        assert_eq!(chop("", 8), vec![""]);
        assert_eq!(cut("abcdef", 4), "abc…");
        assert_eq!(cut("abc", 4), "abc");
        assert_eq!(
            content_text(&json!([{ "type": "text", "text": "a\tb" }, { "x": 1 }])),
            "a    b\n{\"x\":1}"
        );
    }
}
