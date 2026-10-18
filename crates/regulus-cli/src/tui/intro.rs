//! The opening: REGULUS appears letter by letter, glows, and fades into the screen. It
//! never delays anything: the core is reached meanwhile, any key skips it, and
//! `client.intro = false` turns it off (docs/04-client.md §7).

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::palette::{mix, palette};

const WORD: &str = "REGULUS";
/// Five rows per letter, drawn with full blocks.
const GLYPH_ROWS: usize = 5;
const GLYPH_GAP: u16 = 2;
/// Between one letter starting to appear and the next.
const LETTER_EVERY: Duration = Duration::from_millis(60);
/// How long a letter takes to come up to full brightness.
const LETTER_RISE: Duration = Duration::from_millis(160);
/// The shortest time the word stays whole before fading, so it can be read.
const HOLD: Duration = Duration::from_millis(150);
const FADE: Duration = Duration::from_millis(300);

fn glyph(letter: char) -> [&'static str; GLYPH_ROWS] {
    match letter {
        'R' => ["████ ", "█   █", "████ ", "█  █ ", "█   █"],
        'E' => ["█████", "█    ", "████ ", "█    ", "█████"],
        'G' => [" ████", "█    ", "█  ██", "█   █", " ████"],
        'U' => ["█   █", "█   █", "█   █", "█   █", " ███ "],
        'L' => ["█    ", "█    ", "█    ", "█    ", "█████"],
        'S' => [" ████", "█    ", " ███ ", "    █", "████ "],
        _ => ["     "; GLYPH_ROWS],
    }
}

fn ratio(part: Duration, whole: Duration) -> f32 {
    (part.as_secs_f32() / whole.as_secs_f32()).clamp(0.0, 1.0)
}

/// When the last letter is fully up.
pub fn revealed_at() -> Duration {
    LETTER_EVERY * (WORD.len() as u32 - 1) + LETTER_RISE
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Intro {
    /// When the fade started, once whatever the screen waits for is ready.
    fade_from: Option<Duration>,
}

impl Intro {
    /// Called every frame; `ready` says the screen behind has something to show.
    pub fn tick(&mut self, elapsed: Duration, ready: bool) {
        if self.fade_from.is_none() && ready && elapsed >= revealed_at() + HOLD {
            self.fade_from = Some(elapsed);
        }
    }

    pub fn done(&self, elapsed: Duration) -> bool {
        self.fade_from.is_some_and(|from| elapsed >= from + FADE)
    }

    /// Draws the frame at `elapsed`, with `status` under the word.
    pub fn render(&self, elapsed: Duration, status: &str, area: Rect, buf: &mut Buffer) {
        let p = palette();
        buf.set_style(area, Style::default().bg(p.canvas));
        let fade = self
            .fade_from
            .map_or(0.0, |from| ratio(elapsed.saturating_sub(from), FADE));
        let letters: Vec<char> = WORD.chars().collect();
        let width = letters.len() as u16 * 5 + (letters.len() as u16 - 1) * GLYPH_GAP;
        let big = area.width >= width + 2 && area.height >= GLYPH_ROWS as u16 + 4;
        let rows = if big { GLYPH_ROWS as u16 } else { 1 };
        let top = area.y + area.height.saturating_sub(rows + 2) / 2;
        // Each letter rises from the canvas through the focus blue to the text color.
        let color = |index: usize| {
            let start = LETTER_EVERY * index as u32;
            let rise = ratio(elapsed.saturating_sub(start), LETTER_RISE);
            let lit = if rise < 1.0 {
                mix(p.canvas, p.focus, rise)
            } else {
                p.text
            };
            mix(lit, p.canvas, fade)
        };
        if big {
            let left = area.x + area.width.saturating_sub(width) / 2;
            for (index, letter) in letters.iter().enumerate() {
                let x = left + index as u16 * (5 + GLYPH_GAP);
                let style = Style::default().fg(color(index)).bg(p.canvas);
                for (row, line) in glyph(*letter).iter().enumerate() {
                    buf.set_string(x, top + row as u16, line, style);
                }
            }
        } else {
            let spans: Vec<Span> = letters
                .iter()
                .enumerate()
                .map(|(index, letter)| {
                    Span::styled(format!("{letter} "), Style::default().fg(color(index)))
                })
                .collect();
            Paragraph::new(Line::from(spans))
                .alignment(Alignment::Center)
                .render(Rect::new(area.x, top, area.width, 1), buf);
        }
        let under = Rect::new(area.x, top + rows + 1, area.width, 1);
        if under.bottom() <= area.bottom() {
            let shown = ratio(elapsed, revealed_at());
            let style =
                Style::default().fg(mix(mix(p.canvas, p.text_muted, shown), p.canvas, fade));
            Paragraph::new(status)
                .style(style)
                .alignment(Alignment::Center)
                .render(under, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_fits_the_budget_when_the_core_answers_at_once() {
        let mut intro = Intro::default();
        let mut at = Duration::ZERO;
        while !intro.done(at) {
            intro.tick(at, true);
            at += Duration::from_millis(16);
        }
        // docs/00-principles.md: first useful screen in under a second.
        assert!(at < Duration::from_secs(1), "{at:?}");
    }

    #[test]
    fn it_waits_for_the_screen_behind() {
        let mut intro = Intro::default();
        intro.tick(Duration::from_secs(3), false);
        assert!(!intro.done(Duration::from_secs(3)));
        intro.tick(Duration::from_secs(3), true);
        assert!(intro.done(Duration::from_secs(3) + FADE));
    }

    #[test]
    fn the_word_is_drawn_whole_once_revealed() {
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        Intro::default().render(revealed_at(), "connecting", area, &mut buf);
        let text: String = buf.content().iter().map(|cell| cell.symbol()).collect();
        assert_eq!(
            text.matches('█').count(),
            WORD.chars()
                .map(|c| glyph(c).concat().matches('█').count())
                .sum::<usize>()
        );
        assert!(text.contains("connecting"));
    }
}
