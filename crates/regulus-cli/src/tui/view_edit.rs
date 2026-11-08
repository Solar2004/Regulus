//! Drawing the Edit view (workflows and nodes, inspector, wires) and the Templates view.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, ListItem, ListState, Paragraph};
use ratatui::Frame;
use regulus_proto::{Direction, Severity, WireKind, WORKFLOW_SELF};

use super::app::App;
use super::catalog::{self, module_state_of, PieceState};
use super::core::describe;
use super::edit::{end_name, ConfirmFor, Ctx, Editor, InspectorRow, Modal, Pane, TextFor, TreeRow};
use super::palette::palette;
use super::templates::{is_required, require_state, FormRow, RequireState, Templates};
use super::view::{centered, draw_header, list, notice_line, pane, state_span, wrap};

const TREE_WIDTH: u16 = 34;
const WIRES_WIDTH: u16 = 44;
const DIAGNOSTICS_HEIGHT: u16 = 6;
const MODAL_WIDTH: u16 = 76;
const MODAL_HEIGHT: u16 = 20;
const TEMPLATE_LIST_WIDTH: u16 = 34;

fn piece_span(state: PieceState) -> Span<'static> {
    let p = palette();
    let color = match state {
        PieceState::Installed => p.success,
        PieceState::NotInstalled => p.text_muted,
        _ => p.danger,
    };
    let (symbol, word) = state.label();
    state_span(symbol, word.into(), color)
}

fn ctx<'a>(app: &'a App) -> Ctx<'a> {
    Ctx {
        keys: &app.options.keys,
        tuning: &app.options.tuning,
        manifests: match &app.manifests {
            Some(Ok(list)) => list,
            _ => &[],
        },
        modules: &app.modules,
    }
}

pub fn draw_edit(frame: &mut Frame, app: &App, editor: &Editor) {
    let p = palette();
    let area = frame.area();
    frame.render_widget(
        ratatui::widgets::Block::new().style(Style::default().bg(p.canvas).fg(p.text)),
        area,
    );
    let lower = if editor.diagnostics.is_empty() {
        0
    } else {
        DIAGNOSTICS_HEIGHT
    };
    let [header, title, body, diagnostics, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(lower),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(frame, app, header);
    let state = if editor.saved.is_none() {
        state_span("○", "new".into(), p.warning)
    } else if editor.dirty() {
        state_span("●", "unsaved changes".into(), p.warning)
    } else {
        state_span("✓", "saved".into(), p.success)
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" Edit · {} ({}) ", editor.doc.name, editor.doc.id),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            state,
        ])),
        title,
    );
    let [tree, inspector, wires] = Layout::horizontal([
        Constraint::Length(TREE_WIDTH),
        Constraint::Min(30),
        Constraint::Length(WIRES_WIDTH),
    ])
    .areas(body);
    let ctx = ctx(app);
    draw_tree(frame, app, editor, tree);
    draw_inspector(frame, editor, &ctx, inspector);
    draw_wires(frame, editor, wires);
    if !editor.diagnostics.is_empty() {
        let lines: Vec<Line> = editor
            .diagnostics
            .iter()
            .map(|d| {
                let (symbol, color) = match d.severity {
                    Severity::Error => ("✗ error", p.danger),
                    Severity::Warning => ("▲ warning", p.warning),
                    Severity::Info => ("· note", p.text_muted),
                };
                let at: Vec<&str> = [
                    d.workflow_id.as_deref(),
                    d.node_id.as_deref(),
                    d.wire_id.as_deref(),
                ]
                .into_iter()
                .flatten()
                .collect();
                Line::from(vec![
                    Span::styled(format!("{symbol} "), Style::default().fg(color)),
                    Span::styled(format!("{} ", at.join("/")), Style::default().fg(p.steel)),
                    Span::raw(d.message.clone()),
                ])
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).block(pane("Diagnostics", false, p)),
            diagnostics,
        );
    }
    let k = &app.options.keys;
    let keys = format!(
        "{} add · {} delete · {} workflow · {} rename · {} wire · {} remove · {} port · {} save · {} check · {} run · Tab pane · {} back",
        k.add_node.label(),
        k.delete.label(),
        k.new_workflow.label(),
        k.rename.label(),
        k.connect.label(),
        k.remove.label(),
        k.add_port.label(),
        k.save.label(),
        k.check.label(),
        k.run.label(),
        k.back.label()
    );
    frame.render_widget(
        Paragraph::new(notice_line(app, keys)).style(Style::default().bg(p.sidebar)),
        footer,
    );
    if let Some(modal) = &editor.modal {
        draw_modal(frame, editor, &ctx, modal);
    }
}

fn draw_tree(frame: &mut Frame, app: &App, editor: &Editor, area: Rect) {
    let p = palette();
    let items: Vec<ListItem> = editor
        .tree()
        .iter()
        .map(|row| match row {
            TreeRow::Workflow(id) => {
                let workflow = editor.doc.workflow(id);
                let name = workflow.map(|w| w.name.clone()).unwrap_or_default();
                let main = if *id == editor.doc.main_workflow_id {
                    " · main"
                } else {
                    ""
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("▾ {name} "),
                        Style::default().fg(p.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("{id}{main}"), Style::default().fg(p.steel)),
                ]))
            }
            TreeRow::Node(workflow, node) => {
                let node = editor.doc.workflow(workflow).and_then(|w| w.node(node));
                let Some(node) = node else {
                    return ListItem::new("");
                };
                let mut spans = vec![
                    Span::raw(format!("  {} ", node.name)),
                    Span::styled(node.node_type.clone(), Style::default().fg(p.steel)),
                ];
                let state = module_state_of(&app.modules, node.module_id.as_deref());
                if state != PieceState::Installed {
                    spans.push(Span::raw(" "));
                    spans.push(piece_span(state));
                }
                ListItem::new(Line::from(spans))
            }
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(editor.row));
    frame.render_stateful_widget(
        list(items, pane("Workflows", editor.pane == Pane::Tree, p), p),
        area,
        &mut state,
    );
}

fn draw_inspector(frame: &mut Frame, editor: &Editor, ctx: &Ctx, area: Rect) {
    let p = palette();
    let muted = Style::default().fg(p.text_muted);
    let mut head: Vec<Line> = Vec::new();
    if let Some(node) = editor.node() {
        head.push(Line::from(vec![
            Span::styled("type   ", muted),
            Span::raw(node.node_type.clone()),
        ]));
        head.push(Line::from(vec![
            Span::styled("module ", muted),
            Span::raw(
                node.module_id
                    .clone()
                    .unwrap_or_else(|| "(none: a box or resource)".into()),
            ),
            Span::raw(" "),
            piece_span(module_state_of(ctx.modules, node.module_id.as_deref())),
        ]));
        let ports = editor.doc.ports_of(node);
        let named: Vec<String> = ports
            .iter()
            .map(|port| {
                let arrow = if port.direction == Direction::Input {
                    "←"
                } else {
                    "→"
                };
                let kind = if port.kind == WireKind::Uses {
                    "uses"
                } else {
                    "then"
                };
                format!("{arrow}{} {} {kind}", port.id, port.capability)
            })
            .collect();
        head.push(Line::styled(format!("ports  {}", named.join(" · ")), muted));
    } else if let Some(workflow) = editor.workflow() {
        head.push(Line::from(vec![
            Span::styled("workflow ", muted),
            Span::raw(format!("{} · {:?}", workflow.id, workflow.kind).to_lowercase()),
        ]));
        head.push(Line::styled(
            "its own ports, wired inside as self.<port>:",
            muted,
        ));
    }
    let rows = editor.inspector(ctx);
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| match row {
            InspectorRow::Name => {
                let name = editor
                    .node()
                    .map(|n| n.name.clone())
                    .or_else(|| editor.workflow().map(|w| w.name.clone()))
                    .unwrap_or_default();
                ListItem::new(Line::from(vec![
                    Span::styled("name      ", muted),
                    Span::raw(name),
                ]))
            }
            InspectorRow::Field(field) => {
                let shown = catalog::display(field.value.as_ref().or(field.default.as_ref()));
                let kind = field.field_type.clone().unwrap_or_else(|| "raw".into());
                let mark = if field.is_default {
                    Span::styled(" (default)", muted)
                } else {
                    Span::styled(" ●", Style::default().fg(p.warning))
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<10} ", field.id), Style::default().fg(p.text)),
                    Span::raw(shown),
                    mark,
                    Span::styled(format!("  {kind}"), Style::default().fg(p.steel)),
                ]))
            }
            InspectorRow::Internal(inside) => ListItem::new(Line::from(vec![
                Span::styled("inside    ", muted),
                Span::raw(inside.clone().unwrap_or_else(|| "(no workflow)".into())),
            ])),
            InspectorRow::Port(id) => {
                let port = editor.workflow().and_then(|w| w.port(id));
                let text = port.map_or(String::new(), |port| {
                    let dir = if port.direction == Direction::Input {
                        "input "
                    } else {
                        "output"
                    };
                    format!("{WORKFLOW_SELF}.{} {dir} {}", port.id, port.capability)
                });
                ListItem::new(text)
            }
        })
        .collect();
    let focused = editor.pane == Pane::Inspector;
    let block = pane("Inspector", focused, p);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [top, rest] =
        Layout::vertical([Constraint::Length(head.len() as u16), Constraint::Min(1)]).areas(inner);
    frame.render_widget(Paragraph::new(head), top);
    let mut state = ListState::default().with_selected(focused.then_some(editor.field));
    frame.render_stateful_widget(
        list(items, ratatui::widgets::Block::new(), p),
        rest,
        &mut state,
    );
}

fn draw_wires(frame: &mut Frame, editor: &Editor, area: Rect) {
    let p = palette();
    let wires = editor.wires();
    let focused = editor.pane == Pane::Wires;
    if wires.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled("no wires", Style::default().fg(p.text_muted)))
                .block(pane("Wires", focused, p)),
            area,
        );
        return;
    }
    let items: Vec<ListItem> = wires
        .iter()
        .map(|wire| {
            let arrow = if wire.kind == WireKind::Uses {
                "⇢"
            } else {
                "→"
            };
            ListItem::new(format!(
                "{} {arrow} {}",
                end_name(&wire.from),
                end_name(&wire.to)
            ))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(editor.wire));
    frame.render_stateful_widget(list(items, pane("Wires", focused, p), p), area, &mut state);
}

fn draw_modal(frame: &mut Frame, editor: &Editor, ctx: &Ctx, modal: &Modal) {
    let p = palette();
    let area = centered(frame.area(), MODAL_WIDTH, MODAL_HEIGHT);
    frame.render_widget(Clear, area);
    let muted = Style::default().fg(p.text_muted);
    let input_line = |text: &str| {
        Line::from(vec![
            Span::styled("› ", Style::default().fg(p.focus)),
            Span::raw(text.to_string()),
        ])
    };
    match modal {
        Modal::Confirm(purpose) => {
            let question = match purpose {
                ConfirmFor::DeleteNode { node, .. } => format!("Delete node {node} and its wires?"),
                ConfirmFor::DeleteWorkflow(id) => {
                    format!("Delete workflow {id} and the boxes that place it?")
                }
                ConfirmFor::Leave => "Leave without saving? The changes are lost.".to_string(),
                ConfirmFor::Quit => "Quit without saving? The changes are lost.".to_string(),
            };
            let lines = vec![
                Line::raw(question),
                Line::raw(""),
                Line::styled(
                    format!("{} yes · {} no", ctx.keys.yes.label(), ctx.keys.no.label()),
                    muted,
                ),
            ];
            frame.render_widget(
                Paragraph::new(lines).block(pane("Confirm", true, p)),
                centered(area, MODAL_WIDTH, 6),
            );
        }
        Modal::Text { purpose, input } => {
            let (title, hint) = match purpose {
                TextFor::RenameNode { .. } => ("Rename node", "the name shown; the id stays"),
                TextFor::RenameWorkflow(_) => ("Rename workflow", "the name shown; the id stays"),
                TextFor::NewWorkflow => ("New workflow", "its name; the id comes from it"),
                TextFor::Field { .. } => ("Edit field", "Enter keeps it · Esc leaves it as it was"),
                TextFor::NewPort(_) => (
                    "New workflow port",
                    "id [in|out] [capability], e.g. ask in text@1",
                ),
            };
            let lines = vec![Line::styled(hint, muted), Line::raw(""), input_line(input)];
            frame.render_widget(
                Paragraph::new(lines).block(pane(title, true, p)),
                centered(area, MODAL_WIDTH, 6),
            );
        }
        Modal::Choose { options, index, .. } => {
            let items: Vec<ListItem> = options
                .iter()
                .map(|(label, _)| ListItem::new(label.clone()))
                .collect();
            let mut state = ListState::default().with_selected(Some(*index));
            frame.render_stateful_widget(list(items, pane("Choose", true, p), p), area, &mut state);
        }
        Modal::Palette { query, index } => {
            let entries = editor.palette(ctx);
            let shown = catalog::filter_palette(&entries, query);
            let block = pane("Add a node", true, p);
            let inner = block.inner(area);
            frame.render_widget(block, area);
            let [top, rest] =
                Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
            frame.render_widget(
                Paragraph::new(vec![
                    input_line(query),
                    Line::styled("type to filter · ↑↓ · Enter adds", muted),
                ]),
                top,
            );
            if shown.is_empty() {
                let text = if ctx.manifests.is_empty() {
                    "loading the manifests…"
                } else {
                    "nothing matches"
                };
                frame.render_widget(Paragraph::new(Line::styled(text, muted)), rest);
                return;
            }
            let items: Vec<ListItem> = shown
                .iter()
                .map(|entry| {
                    ListItem::new(Line::from(vec![
                        Span::raw(format!("{} ", entry.name)),
                        Span::styled(format!("{} ", entry.detail), Style::default().fg(p.steel)),
                        piece_span(entry.state),
                    ]))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(*index));
            frame.render_stateful_widget(
                list(items, ratatui::widgets::Block::new(), p),
                rest,
                &mut state,
            );
        }
        Modal::Connect { from, index } => {
            let (title, items): (String, Vec<ListItem>) = match from {
                None => (
                    "New wire · from (an output)".into(),
                    editor
                        .sources()
                        .iter()
                        .map(|(end, port)| {
                            ListItem::new(Line::from(vec![
                                Span::raw(format!("{} ", end_name(end))),
                                Span::styled(
                                    format!("{} {:?}", port.capability, port.kind).to_lowercase(),
                                    Style::default().fg(p.steel),
                                ),
                            ]))
                        })
                        .collect(),
                ),
                Some(source) => (
                    format!("New wire · {} → to (an input)", end_name(source)),
                    editor
                        .targets(source)
                        .iter()
                        .map(|(end, port, verdict)| {
                            let state = match verdict {
                                Ok(_) => state_span("✓", "fits".into(), p.success),
                                Err(refusal) => state_span("✗", refusal.reason(), p.text_muted),
                            };
                            ListItem::new(Line::from(vec![
                                Span::raw(format!("{} ", end_name(end))),
                                Span::styled(
                                    format!("{} ", port.capability),
                                    Style::default().fg(p.steel),
                                ),
                                state,
                            ]))
                        })
                        .collect(),
                ),
            };
            let mut state = ListState::default().with_selected(Some(*index));
            frame.render_stateful_widget(list(items, pane(&title, true, p), p), area, &mut state);
        }
    }
}

pub fn draw_templates(frame: &mut Frame, app: &App, view: &Templates) {
    let p = palette();
    let area = frame.area();
    frame.render_widget(
        ratatui::widgets::Block::new().style(Style::default().bg(p.canvas).fg(p.text)),
        area,
    );
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(frame, app, header);
    let [left, right] =
        Layout::horizontal([Constraint::Length(TEMPLATE_LIST_WIDTH), Constraint::Min(30)])
            .areas(body);
    let muted = Style::default().fg(p.text_muted);
    match &view.list {
        None => frame.render_widget(
            Paragraph::new(Line::styled("loading…", muted)).block(pane("Templates", true, p)),
            left,
        ),
        Some(Err(failure)) => frame.render_widget(
            Paragraph::new(Line::styled(
                describe(failure),
                Style::default().fg(p.danger),
            ))
            .block(pane("Templates", true, p)),
            left,
        ),
        Some(Ok(list_of)) if list_of.is_empty() => {
            frame.render_widget(
                Paragraph::new(Line::styled("no templates in [templates] paths", muted))
                    .block(pane("Templates", true, p)),
                left,
            )
        }
        Some(Ok(list_of)) => {
            let items: Vec<ListItem> = list_of
                .iter()
                .map(|t| {
                    let mut spans = vec![Span::raw(t.name.clone())];
                    if t.error.is_some() {
                        spans.push(Span::raw(" "));
                        spans.push(state_span("▲", "unreadable".into(), p.danger));
                    }
                    ListItem::new(Line::from(spans))
                })
                .collect();
            let mut state = ListState::default().with_selected(Some(view.index));
            frame.render_stateful_widget(
                list(items, pane("Templates", view.form.is_none(), p), p),
                left,
                &mut state,
            );
        }
    }
    let width = right.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = Vec::new();
    if let Some(template) = view.selected() {
        lines.extend(
            wrap(&template.description, width)
                .into_iter()
                .map(Line::raw),
        );
        lines.push(Line::styled(template.path.clone(), muted));
        if let Some(error) = &template.error {
            lines.push(Line::styled(
                format!("▲ {error}"),
                Style::default().fg(p.danger),
            ));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "requires",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        for require in &template.requires {
            let state = require_state(require, &app.modules);
            let color = match state {
                RequireState::Installed => p.success,
                RequireState::Missing => p.text_muted,
                RequireState::Broken => p.danger,
            };
            let (symbol, word) = state.label();
            lines.push(Line::from(vec![
                Span::raw(format!("  {} {} ", require.module, require.version)),
                state_span(symbol, word.into(), color),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            "variables",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        let form = view.form.as_ref().filter(|f| f.template == template.name);
        for (at, var) in template.vars.iter().enumerate() {
            let required = if is_required(var) { "*" } else { " " };
            let value = match form {
                Some(form) => form.values[at].clone(),
                None => var
                    .default
                    .clone()
                    .map(|d| format!("default: {d}"))
                    .unwrap_or_else(|| "required".into()),
            };
            let selected = form.is_some_and(|f| f.rows().get(f.row) == Some(&FormRow::Var(at)));
            let mark = if selected { "▸" } else { " " };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{mark}{required}{:<12}", var.name),
                    Style::default().fg(p.text),
                ),
                Span::raw(value),
                Span::styled(format!("  {}", var.description), muted),
            ]));
        }
        if let Some(form) = form {
            let at = form.rows().get(form.row).copied();
            let mark = |row| if at == Some(row) { "▸" } else { " " };
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} {:<12}", mark(FormRow::Id), "id"),
                    Style::default().fg(p.text),
                ),
                Span::raw(form.id.clone()),
                Span::styled("  optional: another id for a second copy", muted),
            ]));
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} {:<12}", mark(FormRow::Force), "replace"),
                    Style::default().fg(p.text),
                ),
                Span::raw(if form.force { "[x]" } else { "[ ]" }),
                Span::styled("  Space: replace a profile with that id", muted),
            ]));
            lines.push(Line::raw(""));
            lines.push(Line::styled(
                "* required · Enter applies · Esc cancels",
                muted,
            ));
        }
    }
    if let Some(Ok(done)) = &view.applied {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            state_span("✓", "applied".into(), p.success),
            Span::raw(format!(" profile {} → {}", done.profile_id, done.path)),
        ]));
        for d in &done.diagnostics {
            lines.push(Line::styled(
                format!("  {:?}: {}", d.severity, d.message).to_lowercase(),
                muted,
            ));
        }
        lines.push(Line::styled(
            format!(
                "{} open in Edit · {} run it",
                app.options.keys.open_in_editor.label(),
                app.options.keys.run.label()
            ),
            Style::default().fg(p.info),
        ));
    } else if let Some(Err(failure)) = &view.applied {
        lines.push(Line::raw(""));
        lines.extend(
            wrap(&describe(failure), width)
                .into_iter()
                .map(|l| Line::styled(l, Style::default().fg(p.danger))),
        );
    }
    let title = if view.form.is_some() {
        "Apply"
    } else {
        "Template"
    };
    frame.render_widget(
        Paragraph::new(lines).block(pane(title, view.form.is_some(), p)),
        right,
    );
    let keys = if view.form.is_some() {
        "type · ↑↓ field · Space toggle · Enter apply · Esc cancel".to_string()
    } else {
        format!(
            "↑↓ move · {} fill in and apply · {} back",
            app.options.keys.open.label(),
            app.options.keys.back.label()
        )
    };
    frame.render_widget(
        Paragraph::new(notice_line(app, keys)).style(Style::default().bg(p.sidebar)),
        footer,
    );
}
