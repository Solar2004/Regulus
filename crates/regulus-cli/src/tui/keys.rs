//! The terminal UI's keys and tunable numbers, with their defaults. Nothing here is fixed
//! in the logic (docs/00-principles.md §1, §4): `mod.rs` may hand in a TOML table, read
//! from `config.toml` once the core's config carries it, and every key it names replaces
//! the default. A binding is a key name (`e`, `/`, `enter`, `ctrl+s`) or a list of them.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use toml::{Table, Value};

/// One key, with or without Ctrl or Alt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
}

impl Chord {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut ctrl = false;
        let mut alt = false;
        let mut rest = text.trim();
        loop {
            let lower = rest.to_ascii_lowercase();
            if lower.starts_with("ctrl+") && rest.len() > 5 {
                ctrl = true;
                rest = &rest[5..];
            } else if lower.starts_with("alt+") && rest.len() > 4 {
                alt = true;
                rest = &rest[4..];
            } else {
                break;
            }
        }
        let code = match rest.to_ascii_lowercase().as_str() {
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "space" => KeyCode::Char(' '),
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            name => {
                let mut chars = rest.chars();
                match (function_key(name), chars.next(), chars.next()) {
                    (Some(number), ..) => KeyCode::F(number),
                    (None, Some(c), None) => {
                        KeyCode::Char(if ctrl { c.to_ascii_lowercase() } else { c })
                    }
                    _ => return Err(format!("`{text}` is not a key")),
                }
            }
        };
        Ok(Self { code, ctrl, alt })
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        let code = match (key.code, self.ctrl) {
            (KeyCode::Char(c), true) => KeyCode::Char(c.to_ascii_lowercase()),
            (code, _) => code,
        };
        code == self.code
            && key.modifiers.contains(KeyModifiers::CONTROL) == self.ctrl
            && key.modifiers.contains(KeyModifiers::ALT) == self.alt
    }

    /// How a footer names it: `e`, `Enter`, `Ctrl+S`.
    pub fn label(&self) -> String {
        let key = match self.code {
            KeyCode::Enter => "Enter".to_string(),
            KeyCode::Esc => "Esc".to_string(),
            KeyCode::Tab => "Tab".to_string(),
            KeyCode::BackTab => "Shift+Tab".to_string(),
            KeyCode::Backspace => "Backspace".to_string(),
            KeyCode::Delete => "Del".to_string(),
            KeyCode::Up => "↑".to_string(),
            KeyCode::Down => "↓".to_string(),
            KeyCode::Left => "←".to_string(),
            KeyCode::Right => "→".to_string(),
            KeyCode::Home => "Home".to_string(),
            KeyCode::End => "End".to_string(),
            KeyCode::PageUp => "PgUp".to_string(),
            KeyCode::PageDown => "PgDn".to_string(),
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::Char(' ') => "Space".to_string(),
            KeyCode::Char(c) if self.ctrl => c.to_ascii_uppercase().to_string(),
            KeyCode::Char(c) => c.to_string(),
            other => format!("{other:?}"),
        };
        let mut out = String::new();
        if self.ctrl {
            out.push_str("Ctrl+");
        }
        if self.alt {
            out.push_str("Alt+");
        }
        out.push_str(&key);
        out
    }
}

/// `f1` to `f12`, as a key name.
fn function_key(name: &str) -> Option<u8> {
    let number: u8 = name.strip_prefix('f')?.parse().ok()?;
    (1..=12).contains(&number).then_some(number)
}

/// Every key one action answers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding(pub Vec<Chord>);

impl Binding {
    fn of(names: &[&str]) -> Self {
        Self(
            names
                .iter()
                .map(|name| Chord::parse(name).expect("default keys parse"))
                .collect(),
        )
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        self.0.iter().any(|chord| chord.matches(key))
    }

    /// The first key, for hints.
    pub fn label(&self) -> String {
        self.0.first().map(Chord::label).unwrap_or_default()
    }

    fn read(value: &Value) -> Result<Self, String> {
        match value {
            Value::String(name) => Ok(Self(vec![Chord::parse(name)?])),
            Value::Array(names) if !names.is_empty() => names
                .iter()
                .map(|name| match name {
                    Value::String(name) => Chord::parse(name),
                    other => Err(format!("`{other}` is not a key name")),
                })
                .collect::<Result<_, _>>()
                .map(Self),
            other => Err(format!("`{other}` is not a key name or a list of them")),
        }
    }
}

macro_rules! keys {
    ($($(#[$doc:meta])* $name:ident = [$($key:literal),+];)+) => {
        /// The keys the Edit, Templates and Code views and the new parts of Operate answer
        /// to.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct Keys { $($(#[$doc])* pub $name: Binding,)+ }

        impl Default for Keys {
            fn default() -> Self {
                Self { $($name: Binding::of(&[$($key),+]),)+ }
            }
        }

        impl Keys {
            /// The defaults with what `table` names. An unknown action, a key that does
            /// not parse or two actions of one context on the same key is an error that
            /// says which.
            pub fn from_table(table: &Table) -> Result<Self, String> {
                let mut keys = Self::default();
                for (action, value) in table {
                    let binding = Binding::read(value).map_err(|e| format!("keys.{action}: {e}"))?;
                    match action.as_str() {
                        $(stringify!($name) => keys.$name = binding,)+
                        other => return Err(format!("keys.{other}: no such action")),
                    }
                }
                keys.check_clashes()?;
                Ok(keys)
            }

            fn binding(&self, action: &str) -> &Binding {
                match action {
                    $(stringify!($name) => &self.$name,)+
                    other => unreachable!("no action {other}"),
                }
            }
        }
    };
}

/// The actions that answer at the same time, so no two of them may share a key. An
/// action that is read in one place only when another has nothing to do (`reject` falls
/// through to `new_profile`) sits in its own context.
const CONTEXTS: &[(&str, &[&str])] = &[
    (
        "operate",
        &[
            "edit",
            "new_profile",
            "templates",
            "filter",
            "send",
            "talk",
            "answer",
            "cancel_turn",
        ],
    ),
    (
        "bind",
        &[
            "up",
            "down",
            "next_pane",
            "previous_pane",
            "open",
            "remove",
            "back",
        ],
    ),
    ("decisions", &["approve", "reject"]),
    (
        "edit",
        &[
            "add_node",
            "delete",
            "new_workflow",
            "rename",
            "connect",
            "remove",
            "add_port",
            "reset",
            "save",
            "check",
            "run",
            "open",
            "back",
            "up",
            "down",
            "next_pane",
            "previous_pane",
        ],
    ),
    (
        "templates",
        &["open", "run", "open_in_editor", "back", "up", "down"],
    ),
    ("dialog", &["yes", "no", "back"]),
    (
        "code",
        &[
            "code_send",
            "code_newline",
            "cancel_turn",
            "code_focus",
            "code_side",
            "code_page_up",
            "code_page_down",
            "code_sessions",
            "code_stop",
            "code_help",
            "code_permission",
            "code_quit",
            "back",
        ],
    ),
    (
        "code_side",
        &[
            "up",
            "down",
            "open",
            "back",
            "code_next_tab",
            "code_previous_tab",
            "code_parent",
            "code_search",
            "code_refresh",
            "code_focus",
            "code_side",
            "cancel_turn",
            "code_page_up",
            "code_page_down",
            "code_sessions",
            "code_stop",
            "code_help",
            "code_permission",
            "code_quit",
        ],
    ),
    (
        "code_permission",
        &["yes", "no", "up", "down", "open", "back"],
    ),
];

impl Keys {
    /// Two actions of one context on the same key: the first pair found, by name.
    fn check_clashes(&self) -> Result<(), String> {
        for (context, actions) in CONTEXTS {
            for (i, first) in actions.iter().enumerate() {
                for second in &actions[i + 1..] {
                    let shared = self
                        .binding(first)
                        .0
                        .iter()
                        .find(|chord| self.binding(second).0.contains(chord));
                    if let Some(chord) = shared {
                        return Err(format!(
                            "keys.{first} and keys.{second} both answer to `{}` in {context}",
                            chord.label()
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

keys! {
    /// Operate: edit the selected profile.
    edit = ["e"];
    /// Operate: a new profile.
    new_profile = ["n"];
    /// Operate: the Templates view.
    templates = ["t"];
    /// Operate: filter the events by node.
    filter = ["/"];
    /// Operate: approve the selected pending decision.
    approve = ["y"];
    /// Operate: reject it. With nothing to reject, the key falls through to the rest
    /// of Operate (`new_profile` by default).
    reject = ["n"];
    /// Operate: send a value to an input of the running workflow.
    send = ["w"];
    /// Operate: talk to the selected execution's workflow through its binding; with none,
    /// or pressed again, choose the binding (docs/04-client.md §7).
    talk = ["c"];
    /// Operate: answer the oldest permission request of the conversation shown.
    answer = ["a"];
    /// Operate: cancel the turn in flight of the conversation shown, also while typing.
    cancel_turn = ["ctrl+x"];
    /// Edit: add a node from the palette.
    add_node = ["a"];
    /// Edit: delete the selected node or workflow.
    delete = ["d"];
    /// Edit: a new workflow.
    new_workflow = ["w"];
    /// Edit: rename the selected node or workflow.
    rename = ["r"];
    /// Edit: a new wire.
    connect = ["c"];
    /// Edit: remove the selected wire or workflow port.
    remove = ["x"];
    /// Edit: a new port of the workflow (`self`).
    add_port = ["p"];
    /// Edit: set a field back to its default.
    reset = ["u"];
    /// Edit: save the profile.
    save = ["ctrl+s"];
    /// Edit: check the saved profile.
    check = ["k"];
    /// Edit and Templates: run the saved profile.
    run = ["ctrl+r"];
    /// Edit an inspector row, or apply a template form.
    open = ["enter"];
    /// Leave a view, a form or a dialog.
    back = ["esc"];
    /// Templates: open the profile just applied in Edit.
    open_in_editor = ["e"];
    /// Dialogs: yes and no.
    yes = ["y"];
    no = ["n"];
    /// Moving through lists and panes.
    up = ["up"];
    down = ["down"];
    next_pane = ["tab", "right"];
    previous_pane = ["backtab", "left"];
    /// Code (`regulus code`, docs/04-client.md §12): send what is typed.
    code_send = ["enter"];
    /// Code: a new line in what is typed.
    code_newline = ["alt+enter", "ctrl+j"];
    /// Code: move between what is typed and the side panel.
    code_focus = ["tab"];
    /// Code: show or hide the side panel.
    code_side = ["ctrl+b"];
    /// Code: scroll the transcript, or the side panel while it has the keys.
    code_page_up = ["pageup"];
    code_page_down = ["pagedown"];
    /// Code: the sessions a person can attach to, and a new one.
    code_sessions = ["ctrl+o"];
    /// Code: stop the execution, after asking.
    code_stop = ["ctrl+t"];
    /// Code: every key and command.
    code_help = ["f1", "alt+h"];
    /// Code: answer the oldest permission request.
    code_permission = ["ctrl+p"];
    /// Code: clear what is typed; with nothing typed, leave.
    code_quit = ["ctrl+c", "ctrl+d"];
    /// Code, side panel: the next and the previous tab.
    code_next_tab = ["right"];
    code_previous_tab = ["left"];
    /// Code, side panel: the folder above, or out of what is open.
    code_parent = ["backspace"];
    /// Code, side panel: search the workspace.
    code_search = ["/"];
    /// Code, side panel: read the tab again.
    code_refresh = ["r"];
}

/// Numbers the TUI uses that a person may want to change.
#[derive(Debug, Clone, PartialEq)]
pub struct Tuning {
    /// `limit` of each `event.list` call.
    pub event_page: usize,
    /// Events kept on screen per execution.
    pub event_keep: usize,
    /// Characters of an event value shown on one line.
    pub value_max: usize,
    /// Gap between nodes placed by the editor, like the app's `cascadeStep`.
    pub cascade_step: f64,
    /// The footprint the app draws a node with, so new nodes do not overlap there.
    pub node_width: f64,
    pub node_height: f64,
    /// Records of a conversation kept on screen (and by `regulus talk` for `/history`),
    /// like the app's `conversationKeep`. 0 keeps everything.
    pub conversation_keep: usize,
    /// Code (`regulus code`): columns of the side panel.
    pub code_side_width: usize,
    /// Code: terminal columns below which the side panel hides on its own.
    pub code_side_min_width: usize,
    /// Code: the most rows the box a person types in grows to.
    pub code_composer_rows: usize,
    /// Code: how often the conversation is read while a turn is in flight, in
    /// milliseconds; the rest follows `client.refresh_ms`.
    pub code_busy_refresh_ms: usize,
    /// Code: how often the mascot's star twinkles while a turn is in flight. 0: never.
    pub code_twinkle_ms: usize,
    /// Code: lines of a tool's output shown under it in the transcript. 0: none.
    pub code_tool_lines: usize,
    /// Code: how long a permission request that just took the keys ignores them, so what
    /// was being typed does not answer it. 0: no wait.
    pub code_permission_grace_ms: usize,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            event_page: 200,
            event_keep: 1000,
            value_max: 240,
            cascade_step: 40.0,
            node_width: 250.0,
            node_height: 160.0,
            conversation_keep: 5000,
            code_side_width: 46,
            code_side_min_width: 110,
            code_composer_rows: 8,
            code_busy_refresh_ms: 250,
            code_twinkle_ms: 400,
            code_tool_lines: 4,
            code_permission_grace_ms: 400,
        }
    }
}

/// Settings that make no sense at 0: a page of no events never advances, a step or a
/// footprint of 0 piles every node on the same spot, a panel or a box of no size shows
/// nothing and a refresh of 0 ms never rests.
const AT_LEAST_ONE: &[&str] = &[
    "event_page",
    "cascade_step",
    "node_width",
    "node_height",
    "code_side_width",
    "code_composer_rows",
    "code_busy_refresh_ms",
];

impl Tuning {
    pub fn from_table(table: &Table) -> Result<Self, String> {
        let mut tuning = Self::default();
        for (name, value) in table {
            let least = if AT_LEAST_ONE.contains(&name.as_str()) {
                1.0
            } else {
                0.0
            };
            let number = match value {
                Value::Integer(n) if *n as f64 >= least => *n as f64,
                Value::Float(n) if *n >= least => *n,
                other => return Err(format!("tui.{name}: `{other}` is not a number ≥ {least}")),
            };
            match name.as_str() {
                "event_page" => tuning.event_page = number as usize,
                "event_keep" => tuning.event_keep = number as usize,
                "value_max" => tuning.value_max = number as usize,
                "cascade_step" => tuning.cascade_step = number,
                "node_width" => tuning.node_width = number,
                "node_height" => tuning.node_height = number,
                "conversation_keep" => tuning.conversation_keep = number as usize,
                "code_side_width" => tuning.code_side_width = number as usize,
                "code_side_min_width" => tuning.code_side_min_width = number as usize,
                "code_composer_rows" => tuning.code_composer_rows = number as usize,
                "code_busy_refresh_ms" => tuning.code_busy_refresh_ms = number as usize,
                "code_twinkle_ms" => tuning.code_twinkle_ms = number as usize,
                "code_tool_lines" => tuning.code_tool_lines = number as usize,
                "code_permission_grace_ms" => tuning.code_permission_grace_ms = number as usize,
                other => return Err(format!("tui.{other}: no such setting")),
            }
        }
        Ok(tuning)
    }
}

/// What the TUI is configured with: `keys` and `tui` of a `[client]`-like table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Options {
    pub keys: Keys,
    pub tuning: Tuning,
}

impl Options {
    pub fn from_client(table: Option<&Table>) -> Result<Self, String> {
        let mut options = Self::default();
        let Some(table) = table else {
            return Ok(options);
        };
        if let Some(keys) = table.get("keys") {
            let keys = keys.as_table().ok_or("keys must be a table")?;
            options.keys = Keys::from_table(keys)?;
        }
        if let Some(tui) = table.get("tui") {
            let tui = tui.as_table().ok_or("tui must be a table")?;
            options.tuning = Tuning::from_table(tui)?;
        }
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn defaults_answer_to_the_documented_keys() {
        let keys = Keys::default();
        assert!(keys
            .edit
            .matches(&key(KeyCode::Char('e'), KeyModifiers::NONE)));
        assert!(keys
            .save
            .matches(&key(KeyCode::Char('s'), KeyModifiers::CONTROL)));
        assert!(!keys
            .save
            .matches(&key(KeyCode::Char('s'), KeyModifiers::NONE)));
        assert!(keys
            .run
            .matches(&key(KeyCode::Char('r'), KeyModifiers::CONTROL)));
        assert!(!keys.run.matches(&key(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(keys.open.matches(&key(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(keys.check_clashes(), Ok(()));
        assert!(keys
            .next_pane
            .matches(&key(KeyCode::Right, KeyModifiers::NONE)));
        assert_eq!(keys.save.label(), "Ctrl+S");
        assert_eq!(keys.filter.label(), "/");
    }

    #[test]
    fn a_table_replaces_only_what_it_names() {
        let table: Table = toml::from_str(
            r#"
            keys = { save = "ctrl+w", connect = ["c", "alt+l"] }
            tui = { event_page = 50, cascade_step = 24 }
            "#,
        )
        .unwrap();
        let options = Options::from_client(Some(&table)).unwrap();
        assert!(options
            .keys
            .save
            .matches(&key(KeyCode::Char('w'), KeyModifiers::CONTROL)));
        assert!(options
            .keys
            .connect
            .matches(&key(KeyCode::Char('l'), KeyModifiers::ALT)));
        assert_eq!(options.keys.edit, Keys::default().edit);
        assert_eq!(options.tuning.event_page, 50);
        assert_eq!(options.tuning.cascade_step, 24.0);
        assert_eq!(options.tuning.event_keep, Tuning::default().event_keep);
    }

    #[test]
    fn mistakes_say_where_they_are() {
        let bad: Table = toml::from_str(r#"keys = { sav = "s" }"#).unwrap();
        assert_eq!(
            Options::from_client(Some(&bad)).unwrap_err(),
            "keys.sav: no such action"
        );
        let bad: Table = toml::from_str(r#"keys = { save = "ctrl+nope" }"#).unwrap();
        assert!(Options::from_client(Some(&bad))
            .unwrap_err()
            .starts_with("keys.save:"));
        let bad: Table = toml::from_str(r#"tui = { event_page = -1 }"#).unwrap();
        assert!(Options::from_client(Some(&bad))
            .unwrap_err()
            .starts_with("tui.event_page"));
        assert_eq!(Options::from_client(None).unwrap(), Options::default());
    }

    #[test]
    fn zero_is_refused_where_it_would_stall() {
        for name in ["event_page", "cascade_step", "node_width", "node_height"] {
            let bad: Table = toml::from_str(&format!("tui = {{ {name} = 0 }}")).unwrap();
            assert_eq!(
                Options::from_client(Some(&bad)).unwrap_err(),
                format!("tui.{name}: `0` is not a number ≥ 1")
            );
        }
        let fine: Table = toml::from_str("tui = { event_keep = 0, value_max = 0 }").unwrap();
        assert_eq!(
            Options::from_client(Some(&fine)).unwrap().tuning.event_keep,
            0
        );
    }

    #[test]
    fn page_and_function_keys_have_names() {
        assert_eq!(Chord::parse("pageup").unwrap().code, KeyCode::PageUp);
        assert_eq!(Chord::parse("PgDn").unwrap().label(), "PgDn");
        assert_eq!(Chord::parse("f1").unwrap().code, KeyCode::F(1));
        assert_eq!(Chord::parse("ctrl+f12").unwrap().label(), "Ctrl+F12");
        // A letter is still a letter, and there is no thirteenth function key.
        assert_eq!(Chord::parse("f").unwrap().code, KeyCode::Char('f'));
        assert!(Chord::parse("f13").is_err());
        assert!(Chord::parse("f0").is_err());
    }

    #[test]
    fn the_coding_screen_is_bound_and_checked_like_the_rest() {
        let keys = Keys::default();
        let enter = key(KeyCode::Enter, KeyModifiers::NONE);
        let alt_enter = key(KeyCode::Enter, KeyModifiers::ALT);
        assert!(keys.code_send.matches(&enter));
        assert!(!keys.code_send.matches(&alt_enter));
        assert!(keys.code_newline.matches(&alt_enter));
        assert!(keys
            .code_newline
            .matches(&key(KeyCode::Char('j'), KeyModifiers::CONTROL)));
        assert!(keys
            .code_help
            .matches(&key(KeyCode::F(1), KeyModifiers::NONE)));
        assert!(keys
            .code_page_up
            .matches(&key(KeyCode::PageUp, KeyModifiers::NONE)));
        assert_eq!(keys.code_quit.label(), "Ctrl+C");
        // Two of its actions on one key are refused by name, as everywhere.
        let bad: Table = toml::from_str(r#"keys = { code_side = "ctrl+x" }"#).unwrap();
        assert_eq!(
            Options::from_client(Some(&bad)).unwrap_err(),
            "keys.cancel_turn and keys.code_side both answer to `Ctrl+X` in code"
        );
        let bad: Table = toml::from_str(r#"keys = { code_refresh = "/" }"#).unwrap();
        assert_eq!(
            Options::from_client(Some(&bad)).unwrap_err(),
            "keys.code_search and keys.code_refresh both answer to `/` in code_side"
        );
        // Its numbers sit with the terminal UI's.
        let fine: Table = toml::from_str(
            r#"
            keys = { code_send = "ctrl+s", code_newline = "enter" }
            tui = { code_side_width = 60, code_tool_lines = 0, code_twinkle_ms = 0 }
            "#,
        )
        .unwrap();
        let options = Options::from_client(Some(&fine)).unwrap();
        assert!(options.keys.code_newline.matches(&enter));
        assert_eq!(options.tuning.code_side_width, 60);
        assert_eq!(options.tuning.code_tool_lines, 0);
        assert_eq!(options.tuning.code_composer_rows, 8);
        for name in [
            "code_side_width",
            "code_composer_rows",
            "code_busy_refresh_ms",
        ] {
            let bad: Table = toml::from_str(&format!("tui = {{ {name} = 0 }}")).unwrap();
            assert_eq!(
                Options::from_client(Some(&bad)).unwrap_err(),
                format!("tui.{name}: `0` is not a number ≥ 1")
            );
        }
    }

    #[test]
    fn two_actions_of_one_context_may_not_share_a_key() {
        let bad: Table = toml::from_str(r#"keys = { add_node = "d" }"#).unwrap();
        assert_eq!(
            Options::from_client(Some(&bad)).unwrap_err(),
            "keys.add_node and keys.delete both answer to `d` in edit"
        );
        let bad: Table = toml::from_str(r#"keys = { run = ["ctrl+r", "enter"] }"#).unwrap();
        assert_eq!(
            Options::from_client(Some(&bad)).unwrap_err(),
            "keys.run and keys.open both answer to `Enter` in edit"
        );
        // Different contexts may: `w` is a new workflow in Edit and a send in Operate.
        let fine: Table = toml::from_str(r#"keys = { send = "w", yes = "y" }"#).unwrap();
        assert!(Options::from_client(Some(&fine)).is_ok());
    }
}
