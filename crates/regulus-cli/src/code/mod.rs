//! Regulus Code in the terminal (docs/04-client.md §12): `regulus code` finds or starts a
//! coding session and opens a screen for it, and `regulus code inspect` reads its
//! workspace. A client like the others: it reaches the core over the socket, knows no
//! module by its name, and what it shows is the conversation the Agent box keeps
//! (`agent.history`) and the workspace the module behind `code.inspect` serves.

pub mod app;
pub mod composer;
#[cfg(test)]
mod fixtures;
pub mod inspect;
pub mod link;
pub mod mascot;
pub mod session;
pub mod settings;
pub mod view;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event as TermEvent,
};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;
use regulus_core::Config;

use self::app::{App, Leave};
use self::link::{Event, Link};
use self::session::Wanted;
use self::settings::Settings;
use crate::tui::keys::Options;

/// How long to wait for a key when nothing was pressed: how often what the core answered
/// reaches the screen.
const IDLE: Duration = Duration::from_millis(100);
/// Keys taken in one go before the screen is drawn again, so a burst of them (text typed
/// ahead, a paste the terminal does not mark) costs one frame, not one each.
const BURST: usize = 256;

/// Opens the coding screen for what `wanted` asks and runs it until the person leaves.
/// Needs a terminal on both ends and a multi-threaded tokio runtime. Says how it was left,
/// for the caller to tell once the terminal is back.
pub async fn run(
    config: Config,
    timeout: Duration,
    wanted: Wanted,
    options: Options,
    settings: Settings,
    skip_form: bool,
) -> Result<Leave> {
    let (events, incoming) = mpsc::channel();
    let link = link::spawn(config.clone(), timeout, events);
    let mut app = App::new(
        wanted,
        options,
        settings,
        config.client.refresh_ms,
        skip_form,
    );
    tokio::task::block_in_place(|| {
        let mut terminal = ratatui::try_init()?;
        // A paste arrives whole, as text: its line ends do not send what is typed.
        let _ = execute!(std::io::stdout(), EnableBracketedPaste);
        // `try_init` restores the terminal if the screen panics; this leaves it as found.
        let restore = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = execute!(std::io::stdout(), DisableBracketedPaste);
            restore(info);
        }));
        let result = screen(&mut terminal, &link, &incoming, &mut app);
        let _ = execute!(std::io::stdout(), DisableBracketedPaste);
        ratatui::restore();
        result
    })
}

fn screen(
    terminal: &mut DefaultTerminal,
    link: &Link,
    incoming: &mpsc::Receiver<Event>,
    app: &mut App,
) -> Result<Leave> {
    let started = Instant::now();
    let mut cache = view::Cache::default();
    let send = |requests: Vec<link::Request>| requests.into_iter().for_each(|r| link.send(r));
    send(app.open());
    loop {
        while let Ok(event) = incoming.try_recv() {
            send(app.apply(event));
        }
        send(app.tick(started.elapsed().as_millis()));
        terminal.draw(|frame| view::draw(frame, app, &mut cache))?;
        app.settle(cache.transcript_room, cache.side_room, cache.page);
        if let Some(leave) = app.leave.take() {
            return Ok(leave);
        }
        let mut wait = IDLE;
        for _ in 0..BURST {
            if !event::poll(wait)? {
                break;
            }
            match event::read()? {
                TermEvent::Key(key) => send(app.key(key)),
                TermEvent::Paste(text) => app.paste(&text),
                _ => {}
            }
            wait = Duration::ZERO;
        }
    }
}
