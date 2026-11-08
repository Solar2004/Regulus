//! The terminal UI: `regulus` with no command, on a terminal (docs/04-client.md §7). A
//! client like the app: it finds or starts the core and shows Operate, Edit and Templates.
//! Every action in it has a command (`run`, `stop`, `ps`, `events`, `send`, `human answer`,
//! `agent prompt`, `msg send`/`log`/`who`, `profile save`, `template apply`).

pub mod app;
pub mod catalog;
pub mod core;
pub mod document;
pub mod edit;
pub mod intro;
pub mod keys;
pub mod palette;
pub mod templates;
pub mod view;
mod view_edit;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event as TermEvent};
use ratatui::DefaultTerminal;
use regulus_core::config::ClientConfig;
use regulus_core::Config;

use self::app::App;
use self::core::{Connection, Core, Event, Request};
use self::intro::Intro;
use self::keys::Options;

/// How often the screen is redrawn while the intro plays.
const FRAME: Duration = Duration::from_millis(16);
/// How long to wait for a key when nothing moves on screen.
const IDLE: Duration = Duration::from_millis(100);

/// Runs until the person quits, with the default keys. Needs a multi-threaded tokio
/// runtime.
pub async fn run(config: Config, timeout: Duration) -> Result<()> {
    run_with(config, timeout, None).await
}

/// Like [`run`], with keys and numbers from `client`: a table shaped like `[client]` of
/// `config.toml` whose `keys` and `tui` tables replace the defaults (`keys.rs`).
pub async fn run_with(
    config: Config,
    timeout: Duration,
    client: Option<&toml::Table>,
) -> Result<()> {
    let options = options(&config, client)?;
    let (events, incoming) = mpsc::channel();
    let core = core::spawn(config.clone(), timeout, events);
    core.send(Request::Status);
    let refresh = Duration::from_millis(config.client.refresh_ms);
    let intro = config.client.intro;
    tokio::task::block_in_place(|| {
        let mut terminal = ratatui::try_init()?;
        let result = screen(&mut terminal, &core, &incoming, refresh, intro, options);
        ratatui::restore();
        result
    })
}

/// `[client]` of `config.toml` as [`run_with`] takes it: its `keys` and `tui` tables.
pub fn client_table(client: &ClientConfig) -> toml::Table {
    let mut table = toml::Table::new();
    table.insert("keys".into(), toml::Value::Table(client.keys.clone()));
    table.insert("tui".into(), toml::Value::Table(client.tui.clone()));
    table
}

/// The keys and numbers `client` sets. A mistake stops the start with the file and the
/// key it is in, before the terminal is taken.
pub fn options(config: &Config, client: Option<&toml::Table>) -> Result<Options> {
    Options::from_client(client)
        .map_err(|e| anyhow::anyhow!("{}: [client] {e}", config.config_path().display()))
}

fn intro_status(connection: &Connection) -> &'static str {
    match connection {
        Connection::Connecting => "connecting to the core",
        Connection::Starting => "starting the core",
        Connection::Connected { .. } => "connected",
        Connection::Failed(_) => "no core answered",
    }
}

fn screen(
    terminal: &mut DefaultTerminal,
    core: &Core,
    incoming: &mpsc::Receiver<Event>,
    refresh: Duration,
    show_intro: bool,
    options: Options,
) -> Result<()> {
    let started = Instant::now();
    let mut intro = show_intro.then(Intro::default);
    let mut app = App::new(options);
    let mut refreshed = Instant::now();
    let mut watched = None;
    let mut polled = None;
    loop {
        while let Ok(event) = incoming.try_recv() {
            for request in app.apply(event) {
                core.send(request);
            }
        }
        let elapsed = started.elapsed();
        if let Some(playing) = &mut intro {
            let ready = !matches!(
                app.connection,
                Connection::Connecting | Connection::Starting
            );
            playing.tick(elapsed, ready);
            if playing.done(elapsed) {
                intro = None;
            }
        }
        terminal.draw(|frame| match &intro {
            Some(playing) => playing.render(
                elapsed,
                intro_status(&app.connection),
                frame.area(),
                frame.buffer_mut(),
            ),
            None => view::draw(frame, &app),
        })?;
        if app.quit {
            return Ok(());
        }
        if event::poll(if intro.is_some() { FRAME } else { IDLE })? {
            if let TermEvent::Key(key) = event::read()? {
                if intro.take().is_none() {
                    for request in app.key(key) {
                        core.send(request);
                    }
                }
            }
        }
        // A newly selected channel is loaded at once, not on the next refresh.
        let channel = app.watched_channel();
        if channel != watched {
            if let Some(request) = channel.clone() {
                core.send(request);
            }
            watched = channel;
        }
        // So are the events and decisions of a newly selected execution.
        let poll = app.poll_key();
        let tick = refreshed.elapsed() >= refresh;
        if poll != polled || tick {
            for request in app.poll() {
                core.send(request);
            }
            polled = poll;
        }
        if tick {
            core.send(Request::Status);
            if let Some(request) = watched.clone() {
                core.send(request);
            }
            refreshed = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn home(name: &str, text: &str) -> std::path::PathBuf {
        let home = std::env::temp_dir().join(format!("regulus-tui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.toml"), text).unwrap();
        home
    }

    #[test]
    fn config_toml_reaches_the_options() {
        let home = home(
            "options",
            "[client.keys]\nsave = \"ctrl+w\"\n[client.tui]\nevent_page = 50\n",
        );
        let config = Config::load(&home).unwrap();
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
        let options = options(&config, Some(&client_table(&config.client))).unwrap();
        let ctrl_w = KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert!(options.keys.save.matches(&ctrl_w));
        assert_eq!(options.tuning.event_page, 50);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_mistake_names_the_file_and_the_key() {
        let home = home("mistake", "[client.tui]\nevent_pag = 50\n");
        let config = Config::load(&home).unwrap();
        let error = options(&config, Some(&client_table(&config.client)))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("config.toml: [client] tui.event_pag"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
