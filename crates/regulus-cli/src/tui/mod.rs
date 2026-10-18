//! The terminal UI: `regulus` with no command, on a terminal (docs/04-client.md §7). A
//! client like the app: it finds or starts the core and shows the Operate view. Every
//! action in it has a command (`run`, `stop`, `ps`, `agent prompt`, `msg send`/`log`/`who`).

pub mod app;
pub mod core;
pub mod intro;
pub mod palette;
pub mod view;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event as TermEvent};
use ratatui::DefaultTerminal;
use regulus_core::Config;

use self::app::App;
use self::core::{Connection, Core, Event, Request};
use self::intro::Intro;

/// How often the screen is redrawn while the intro plays.
const FRAME: Duration = Duration::from_millis(16);
/// How long to wait for a key when nothing moves on screen.
const IDLE: Duration = Duration::from_millis(100);

/// Runs until the person quits. Needs a multi-threaded tokio runtime.
pub async fn run(config: Config, timeout: Duration) -> Result<()> {
    let (events, incoming) = mpsc::channel();
    let core = core::spawn(config.clone(), timeout, events);
    core.send(Request::Status);
    let refresh = Duration::from_millis(config.client.refresh_ms);
    let intro = config.client.intro;
    tokio::task::block_in_place(|| {
        let mut terminal = ratatui::try_init()?;
        let result = screen(&mut terminal, &core, &incoming, refresh, intro);
        ratatui::restore();
        result
    })
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
) -> Result<()> {
    let started = Instant::now();
    let mut intro = show_intro.then(Intro::default);
    let mut app = App::default();
    let mut refreshed = Instant::now();
    let mut watched = None;
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
        if refreshed.elapsed() >= refresh {
            core.send(Request::Status);
            if let Some(request) = watched.clone() {
                core.send(request);
            }
            refreshed = Instant::now();
        }
    }
}
