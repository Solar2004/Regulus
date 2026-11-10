//! What `[client.code]` of `config.toml` sets for Regulus Code (docs/04-client.md §12).
//! The table is shared with the app: a key this client does not know belongs to another
//! client and is left alone, but a known key with a value that makes no sense stops the
//! start with its name. Keys and numbers of the screen live with the rest of the terminal
//! UI's, in `[client.keys]` and `[client.tui]` (`tui/keys.rs`).

use toml::{Table, Value};

/// The table of `[client]` these settings are read from.
pub const SECTION: &str = "code";

/// What happens to the execution when the person leaves the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnExit {
    /// It goes on; `regulus code` attaches to it again.
    Keep,
    Stop,
    /// A dialog asks.
    Ask,
}

/// When the setup form is shown before a profile is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupForm {
    /// Whenever a launch writes a profile.
    Always,
    /// Only when the launch cannot go on as it is: a value is missing or the core refused.
    Needed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// What the product is called on screen.
    pub label: String,
    /// The template a launch applies.
    pub template: String,
    /// Draw the mascot.
    pub mascot: bool,
    pub on_exit: OnExit,
    pub setup: SetupForm,
    /// How often the workspace shown in the side panel is read again, in milliseconds.
    /// 0: only when a turn ends, a tool finishes or the person asks.
    pub inspect_refresh_ms: u64,
    /// Show the session details row above the conversation.
    pub status_bar: bool,
    /// Session details included in that row, in display order.
    pub status_fields: Vec<StatusField>,
    /// Start with the workspace panel open.
    pub side_panel: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusField {
    Root,
    Model,
    Branch,
    State,
    Session,
}

impl StatusField {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "root" => Some(Self::Root),
            "model" => Some(Self::Model),
            "branch" => Some(Self::Branch),
            "state" => Some(Self::State),
            "session" => Some(Self::Session),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Model => "model",
            Self::Branch => "branch",
            Self::State => "state",
            Self::Session => "session",
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            label: "Regulus Code".into(),
            template: "regulus-code".into(),
            mascot: true,
            on_exit: OnExit::Keep,
            setup: SetupForm::Always,
            inspect_refresh_ms: 4000,
            status_bar: true,
            status_fields: vec![
                StatusField::Root,
                StatusField::Model,
                StatusField::Branch,
                StatusField::State,
                StatusField::Session,
            ],
            side_panel: false,
        }
    }
}

fn text(key: &str, value: &Value) -> Result<String, String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Ok(text.trim().to_string()),
        other => Err(format!("{SECTION}.{key}: `{other}` is not a text")),
    }
}

impl Settings {
    /// The defaults with what `[client]` writes under `code`. `client` is `[client]` as
    /// written (`ClientConfig::written`).
    pub fn from_client(client: &Table) -> Result<Self, String> {
        let mut settings = Self::default();
        let table = match client.get(SECTION) {
            None => return Ok(settings),
            Some(Value::Table(table)) => table,
            Some(_) => return Err(format!("{SECTION} must be a table")),
        };
        for (key, value) in table {
            match key.as_str() {
                "label" => settings.label = text(key, value)?,
                "template" => settings.template = text(key, value)?,
                "mascot" => {
                    settings.mascot = value
                        .as_bool()
                        .ok_or_else(|| format!("{SECTION}.{key}: `{value}` is not true or false"))?
                }
                "on_exit" => {
                    settings.on_exit = match value.as_str() {
                        Some("keep") => OnExit::Keep,
                        Some("stop") => OnExit::Stop,
                        Some("ask") => OnExit::Ask,
                        _ => {
                            return Err(format!(
                                "{SECTION}.{key}: `{value}` is not keep, stop or ask"
                            ))
                        }
                    }
                }
                "setup" => {
                    settings.setup = match value.as_str() {
                        Some("always") => SetupForm::Always,
                        Some("needed") => SetupForm::Needed,
                        _ => {
                            return Err(format!(
                                "{SECTION}.{key}: `{value}` is not always or needed"
                            ))
                        }
                    }
                }
                "inspect_refresh_ms" => {
                    settings.inspect_refresh_ms = value
                        .as_integer()
                        .and_then(|n| u64::try_from(n).ok())
                        .ok_or_else(|| format!("{SECTION}.{key}: `{value}` is not a number ≥ 0"))?
                }
                "status_bar" => {
                    settings.status_bar = value
                        .as_bool()
                        .ok_or_else(|| format!("{SECTION}.{key}: `{value}` is not true or false"))?
                }
                "side_panel" => {
                    settings.side_panel = value
                        .as_bool()
                        .ok_or_else(|| format!("{SECTION}.{key}: `{value}` is not true or false"))?
                }
                "status_fields" => {
                    let fields = value.as_array().ok_or_else(|| {
                        format!("{SECTION}.{key}: `{value}` is not a list of fields")
                    })?;
                    settings.status_fields = fields
                        .iter()
                        .map(|field| {
                            let name = field.as_str().ok_or_else(|| {
                                format!("{SECTION}.{key}: `{field}` is not a field name")
                            })?;
                            StatusField::parse(name).ok_or_else(|| {
                                format!(
                                    "{SECTION}.{key}: `{name}` is unknown (use root, model, branch, state or session)"
                                )
                            })
                        })
                        .collect::<Result<_, _>>()?;
                }
                // Another client's key (the app keeps `panels` here): not ours to judge.
                _ => {}
            }
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(text: &str) -> Table {
        toml::from_str(text).unwrap()
    }

    #[test]
    fn nothing_written_is_every_default() {
        assert_eq!(
            Settings::from_client(&Table::new()).unwrap(),
            Settings::default()
        );
        let settings = Settings::default();
        assert_eq!(settings.label, "Regulus Code");
        assert_eq!(settings.template, "regulus-code");
        assert!(settings.mascot);
        assert_eq!(settings.on_exit, OnExit::Keep);
    }

    #[test]
    fn what_is_written_replaces_the_default_and_other_clients_keys_are_left_alone() {
        let settings = Settings::from_client(&client(
            r#"
            [code]
            label = "Leo"
            template = "my-code"
            mascot = false
            on_exit = "ask"
            setup = "needed"
            inspect_refresh_ms = 0
            status_bar = false
            status_fields = ["branch", "state"]
            side_panel = true
            panels = ["files", "diff"]
            "#,
        ))
        .unwrap();
        assert_eq!(settings.label, "Leo");
        assert_eq!(settings.template, "my-code");
        assert!(!settings.mascot);
        assert_eq!(settings.on_exit, OnExit::Ask);
        assert_eq!(settings.setup, SetupForm::Needed);
        assert_eq!(settings.inspect_refresh_ms, 0);
        assert!(!settings.status_bar);
        assert_eq!(
            settings.status_fields,
            vec![StatusField::Branch, StatusField::State]
        );
        assert!(settings.side_panel);
    }

    #[test]
    fn a_value_that_makes_no_sense_names_its_key() {
        for (text, said) in [
            ("code = 3", "code must be a table"),
            ("[code]\nlabel = \"\"", "code.label: `\"\"` is not a text"),
            (
                "[code]\nmascot = \"yes\"",
                "code.mascot: `\"yes\"` is not true or false",
            ),
            (
                "[code]\non_exit = \"kill\"",
                "code.on_exit: `\"kill\"` is not keep, stop or ask",
            ),
            (
                "[code]\ninspect_refresh_ms = -1",
                "code.inspect_refresh_ms: `-1` is not a number ≥ 0",
            ),
            (
                "[code]\nstatus_fields = [\"workers\"]",
                "code.status_fields: `workers` is unknown (use root, model, branch, state or session)",
            ),
        ] {
            assert_eq!(Settings::from_client(&client(text)).unwrap_err(), said);
        }
    }
}
