//! The slash commands of the coding screen, in one place (docs/04-client.md §12): what is
//! typed after `/`, the other words that mean the same, what it takes and what it does.
//! Running a command (`app.rs`), the help and the menu of suggestions (`view.rs`) all read
//! [`COMMANDS`]: a command that is listed is a command that runs, because `app.rs` runs
//! them by [`Command`] with no catch-all arm.
//!
//! The menu is what opens while a lone `/word` is typed. It only ever completes the text;
//! sending it is still Enter on a whole command, so nothing runs from a half-typed word.

use ratatui::crossterm::event::KeyEvent;

use crate::tui::keys::Keys;

/// What a command does when it is run. `app.rs` has one arm for each and no wildcard, so a new one does
/// not compile until it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Files,
    Read,
    Search,
    Diff,
    Workers,
    Activity,
    Attach,
    Sessions,
    Stop,
    Cancel,
    /// `/allow`, `/reject`: an answer to a permission request. Any other word is tried as
    /// one of its option ids, so it is not listed.
    Answer,
    History,
    Help,
    Quit,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Spec {
    pub command: Command,
    /// What follows `/` to run it.
    pub name: &'static str,
    /// Other words that run the same thing.
    pub aliases: &'static [&'static str],
    /// What it takes after the name: `<x>` is needed, `[x]` is optional; empty: nothing.
    pub arguments: &'static str,
    pub description: &'static str,
}

impl Spec {
    /// `/files [folder]`.
    pub fn usage(&self) -> String {
        if self.arguments.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} {}", self.name, self.arguments)
        }
    }

    /// The name and then the aliases.
    pub fn words(&self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.name).chain(self.aliases.iter().copied())
    }

    /// `word` is exactly the name or one of the aliases.
    pub fn answers_to(&self, word: &str) -> bool {
        self.words().any(|w| w == word)
    }

    pub fn takes_argument(&self) -> bool {
        !self.arguments.is_empty()
    }
}

/// Every command, in the order the menu and the help list them.
pub const COMMANDS: &[Spec] = &[
    Spec {
        command: Command::Files,
        name: "files",
        aliases: &["ls"],
        arguments: "[folder]",
        description: "list a folder of the workspace",
    },
    Spec {
        command: Command::Read,
        name: "read",
        aliases: &["open"],
        arguments: "<file>",
        description: "open a file",
    },
    Spec {
        command: Command::Search,
        name: "search",
        aliases: &["grep"],
        arguments: "<text>",
        description: "search the workspace",
    },
    Spec {
        command: Command::Diff,
        name: "diff",
        aliases: &["changes"],
        arguments: "",
        description: "what changed",
    },
    Spec {
        command: Command::Workers,
        name: "workers",
        aliases: &[],
        arguments: "",
        description: "the executions the session started",
    },
    Spec {
        command: Command::Activity,
        name: "activity",
        aliases: &[],
        arguments: "",
        description: "the session, the plan and the tools of the turn",
    },
    Spec {
        command: Command::Attach,
        name: "attach",
        aliases: &[],
        arguments: "[file]",
        description: "add a file to the draft; it is not sent yet",
    },
    Spec {
        command: Command::Sessions,
        name: "sessions",
        aliases: &[],
        arguments: "",
        description: "other sessions, or a new one",
    },
    Spec {
        command: Command::Cancel,
        name: "cancel",
        aliases: &[],
        arguments: "",
        description: "cancel the turn in flight",
    },
    Spec {
        command: Command::Stop,
        name: "stop",
        aliases: &[],
        arguments: "",
        description: "stop the execution (asks first)",
    },
    Spec {
        command: Command::Answer,
        name: "allow",
        aliases: &[],
        arguments: "[request]",
        description: "allow a permission request; /<option id> picks an option",
    },
    Spec {
        command: Command::Answer,
        name: "reject",
        aliases: &[],
        arguments: "[request]",
        description: "reject a permission request",
    },
    Spec {
        command: Command::History,
        name: "history",
        aliases: &[],
        arguments: "",
        description: "go to where the conversation starts",
    },
    Spec {
        command: Command::Help,
        name: "help",
        aliases: &[],
        arguments: "",
        description: "every key and command",
    },
    Spec {
        command: Command::Quit,
        name: "quit",
        aliases: &["exit"],
        arguments: "",
        description: "leave",
    },
];

/// The command a word runs.
pub fn find(word: &str) -> Option<&'static Spec> {
    COMMANDS.iter().find(|spec| spec.answers_to(word))
}

/// The word being typed, when `text` is nothing but `/` and that word: no arguments yet, no
/// new line, and not `//`, which sends a message that starts with a slash.
pub fn word(text: &str) -> Option<&str> {
    let word = text.strip_prefix('/')?;
    (!word.starts_with('/') && !word.contains(char::is_whitespace)).then_some(word)
}

/// The commands whose name or an alias starts with what is typed, whole words first.
pub fn suggest(text: &str) -> Vec<&'static Spec> {
    let Some(typed) = word(text) else {
        return Vec::new();
    };
    let lower = typed.to_lowercase();
    let mut found: Vec<&'static Spec> = COMMANDS
        .iter()
        .filter(|spec| spec.words().any(|w| w.starts_with(&lower)))
        .collect();
    found.sort_by_key(|spec| !spec.answers_to(typed));
    found
}

/// What the box holds once `spec` is chosen for `text`: the word it was typed as if that
/// was already one of its own, the name if not, and a space when arguments come next.
pub fn completion(spec: &Spec, text: &str) -> String {
    let typed = word(text).unwrap_or_default();
    let name = if spec.answers_to(typed) {
        typed
    } else {
        spec.name
    };
    if spec.takes_argument() {
        format!("/{name} ")
    } else {
        format!("/{name}")
    }
}

/// What the menu did with a key.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not the menu's: the key goes on as if it was not there.
    Ignored,
    /// Taken, and it changed nothing in the box.
    Handled,
    /// Taken: this is the text the box takes now.
    Complete(String),
}

/// The menu of suggestions while a command is being typed: which one is chosen, and whether
/// it was closed for the text now in the box. The suggestions are not kept: they are the
/// text's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Menu {
    selected: usize,
    /// The text it was closed on; any other text opens it again.
    dismissed: Option<String>,
}

impl Menu {
    /// What the menu offers for `text`: nothing when it was closed for it.
    pub fn entries(&self, text: &str) -> Vec<&'static Spec> {
        if self.dismissed.as_deref() == Some(text) {
            return Vec::new();
        }
        suggest(text)
    }

    /// The chosen row among `len`.
    pub fn selected(&self, len: usize) -> usize {
        self.selected.min(len.saturating_sub(1))
    }

    /// The text changed: the first row is chosen again and the menu may open.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The keys of the menu, which has them only while it offers something. `answers` are
    /// the option ids of the permission requests that wait: typed as `/id`, they are an
    /// answer, not a command to complete.
    ///
    /// Up and down choose (with one row, they go on to the earlier prompts), the completion
    /// key completes, close closes, and send completes unless the box already holds a whole
    /// command: a command is sent only by sending what was typed in full.
    pub fn key(&mut self, key: &KeyEvent, keys: &Keys, text: &str, answers: &[String]) -> Outcome {
        let entries = self.entries(text);
        let at = self.selected(entries.len());
        let Some(spec) = entries.get(at).copied() else {
            return Outcome::Ignored;
        };
        if keys.back.matches(key) {
            self.dismissed = Some(text.to_string());
            return Outcome::Handled;
        }
        if keys.code_complete.matches(key) {
            return Outcome::Complete(completion(spec, text));
        }
        if keys.code_send.matches(key) {
            let typed = word(text).unwrap_or_default();
            let whole = spec.answers_to(typed) || answers.iter().any(|id| id == typed);
            return if whole {
                Outcome::Ignored
            } else {
                Outcome::Complete(completion(spec, text))
            };
        }
        let by = if keys.up.matches(key) {
            -1
        } else if keys.down.matches(key) {
            1
        } else {
            return Outcome::Ignored;
        };
        if entries.len() < 2 {
            return Outcome::Ignored;
        }
        self.selected = at.saturating_add_signed(by).min(entries.len() - 1);
        Outcome::Handled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn names(text: &str) -> Vec<&'static str> {
        suggest(text).iter().map(|spec| spec.name).collect()
    }

    #[test]
    fn the_registry_is_consistent() {
        let mut seen = std::collections::HashSet::new();
        for spec in COMMANDS {
            for word in spec.words() {
                assert!(seen.insert(word), "`{word}` is the word of two commands");
                assert!(
                    !word.is_empty() && !word.contains(char::is_whitespace) && word.is_ascii(),
                    "`{word}` cannot be typed as one word"
                );
            }
            assert!(!spec.description.is_empty(), "{} says nothing", spec.name);
            assert!(spec.usage().starts_with(&format!("/{}", spec.name)));
        }
        assert_eq!(find("ls").map(|s| s.name), Some("files"));
        assert_eq!(find("exit").map(|s| s.command), Some(Command::Quit));
        assert!(find("nope").is_none());
    }

    #[test]
    fn only_a_lone_word_after_a_slash_is_filtered() {
        assert_eq!(names("/").len(), COMMANDS.len());
        assert_eq!(names("/di"), ["diff"]);
        // Aliases count, and the command shows once.
        assert_eq!(names("/ch"), ["diff"]);
        assert_eq!(names("/c"), ["diff", "cancel"]);
        assert_eq!(names("/DI"), ["diff"]);
        // Whole words first.
        assert_eq!(names("/ls"), ["files"]);
        // Arguments, a message that starts with a slash and plain text are not commands.
        for text in [
            "/files src",
            "/files ",
            "//files",
            "files",
            "",
            " /fi",
            "/fi\nx",
        ] {
            assert!(suggest(text).is_empty(), "`{text}` is not a command");
        }
        assert!(suggest("/zzz").is_empty());
    }

    #[test]
    fn completing_keeps_what_was_typed_when_it_is_a_command() {
        let files = find("files").unwrap();
        let diff = find("diff").unwrap();
        // A command that takes arguments leaves room for them.
        assert_eq!(completion(files, "/fi"), "/files ");
        assert_eq!(completion(diff, "/di"), "/diff");
        assert_eq!(completion(files, "/ls"), "/ls ");
        assert_eq!(completion(find("quit").unwrap(), "/exit"), "/exit");
        assert_eq!(completion(diff, "/DI"), "/diff");
    }

    #[test]
    fn the_menu_keys_choose_complete_and_close() {
        let keys = Keys::default();
        let mut menu = Menu::default();
        let text = "/s";
        let rows = menu.entries(text);
        assert_eq!(
            rows.iter().map(|s| s.name).collect::<Vec<_>>(),
            ["search", "sessions", "stop"]
        );
        assert_eq!(
            menu.key(&press(KeyCode::Down), &keys, text, &[]),
            Outcome::Handled
        );
        assert_eq!(menu.selected(rows.len()), 1);
        assert_eq!(
            menu.key(&press(KeyCode::Down), &keys, text, &[]),
            Outcome::Handled
        );
        // The ends hold.
        assert_eq!(
            menu.key(&press(KeyCode::Down), &keys, text, &[]),
            Outcome::Handled
        );
        assert_eq!(menu.selected(rows.len()), 2);
        assert_eq!(
            menu.key(&press(KeyCode::Up), &keys, text, &[]),
            Outcome::Handled
        );
        // Tab and Enter complete the chosen one and nothing else.
        for code in [KeyCode::Tab, KeyCode::Enter] {
            assert_eq!(
                menu.key(&press(code), &keys, text, &[]),
                Outcome::Complete("/sessions".into())
            );
        }
        // Keys that are not its own go on.
        assert_eq!(
            menu.key(&press(KeyCode::Left), &keys, text, &[]),
            Outcome::Ignored
        );
        assert_eq!(
            menu.key(&press(KeyCode::Char('x')), &keys, text, &[]),
            Outcome::Ignored
        );
        // Close: for this text only.
        assert_eq!(
            menu.key(&press(KeyCode::Esc), &keys, text, &[]),
            Outcome::Handled
        );
        assert!(menu.entries(text).is_empty());
        assert_eq!(
            menu.key(&press(KeyCode::Tab), &keys, text, &[]),
            Outcome::Ignored
        );
        assert!(!menu.entries("/se").is_empty());
        menu.reset();
        assert_eq!(menu.entries(text).len(), 3);
    }

    #[test]
    fn enter_sends_a_whole_command_and_completes_the_rest() {
        let keys = Keys::default();
        let enter = press(KeyCode::Enter);
        let mut menu = Menu::default();
        for whole in ["/stop", "/files", "/ls", "/quit"] {
            assert_eq!(
                menu.key(&enter, &keys, whole, &[]),
                Outcome::Ignored,
                "{whole}"
            );
        }
        assert_eq!(
            menu.key(&enter, &keys, "/sto", &[]),
            Outcome::Complete("/stop".into())
        );
        // The id of an option of a request that waits is an answer, not a half command.
        assert_eq!(
            menu.key(&enter, &keys, "/al", &["al".into()]),
            Outcome::Ignored
        );
        // No match and arguments: plain input.
        assert_eq!(menu.key(&enter, &keys, "/zzz", &[]), Outcome::Ignored);
        assert_eq!(menu.key(&enter, &keys, "/files src", &[]), Outcome::Ignored);
    }

    #[test]
    fn one_row_leaves_up_and_down_to_the_earlier_prompts() {
        let keys = Keys::default();
        let mut menu = Menu::default();
        for code in [KeyCode::Up, KeyCode::Down] {
            assert_eq!(
                menu.key(&press(code), &keys, "/diff", &[]),
                Outcome::Ignored
            );
        }
    }

    #[test]
    fn the_completion_key_is_the_configured_one() {
        let table: toml::Table = toml::from_str(r#"keys = { code_complete = "ctrl+y" }"#).unwrap();
        let keys = crate::tui::keys::Options::from_client(Some(&table))
            .unwrap()
            .keys;
        let mut menu = Menu::default();
        let ctrl_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(
            menu.key(&ctrl_y, &keys, "/di", &[]),
            Outcome::Complete("/diff".into())
        );
        assert_eq!(
            menu.key(&press(KeyCode::Tab), &keys, "/di", &[]),
            Outcome::Ignored
        );
    }
}
