//! The mascot of Regulus Code: a small lion seen from the front whose mane is a star burst
//! and whose nose is the star Regulus, the heart of the Lion (docs/04-client.md §12).
//! Plain ASCII, seven columns by three rows, so it reads the same in any terminal:
//!
//! ```text
//!  \\|//
//! ={o*o}=
//!  //|\\
//! ```
//!
//! Only the middle row changes with what is going on, and a word always says the same
//! next to it: a state is never the drawing alone.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::tui::palette::Palette;

/// Columns and rows of the drawing.
pub const WIDTH: u16 = 7;
pub const HEIGHT: u16 = 3;
/// The mane above and below the face: five rays each.
const UPPER: &str = " \\\\|// ";
const LOWER: &str = " //|\\\\ ";
/// The drawing in one line, for tight places: the mane and the star.
pub const GLYPH: &str = "{*}";

/// What the face says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mood {
    Idle,
    /// A turn is in flight: the star twinkles.
    Working,
    /// A permission request waits for the person.
    Asking,
    /// A turn failed, or there is no core.
    Failed,
    /// The execution is not running.
    Asleep,
}

/// The eyes and the nose of a mood. `beat` is which half of a twinkle it is.
fn face(mood: Mood, beat: bool) -> (char, char) {
    match mood {
        Mood::Idle => ('o', '*'),
        Mood::Working if beat => ('o', '+'),
        Mood::Working => ('o', '*'),
        Mood::Asking => ('o', '?'),
        Mood::Failed => ('x', '*'),
        Mood::Asleep => ('-', '*'),
    }
}

/// The three rows as plain text, as wide as [`WIDTH`].
pub fn rows(mood: Mood, beat: bool) -> [String; HEIGHT as usize] {
    let (eye, nose) = face(mood, beat);
    [
        UPPER.to_string(),
        format!("={{{eye}{nose}{eye}}}="),
        LOWER.to_string(),
    ]
}

/// The three rows in the palette: the mane in steel, the whiskers muted, the eyes in the
/// text color and the star in the focus blue, the only accent.
pub fn lines(mood: Mood, beat: bool, p: &Palette) -> Vec<Line<'static>> {
    let (eye, nose) = face(mood, beat);
    let mane = Style::default().fg(p.steel);
    let eyes = Style::default().fg(match mood {
        Mood::Failed => p.danger,
        Mood::Asleep => p.text_muted,
        _ => p.text,
    });
    let star = Style::default()
        .fg(match mood {
            Mood::Asking => p.warning,
            _ => p.focus,
        })
        .add_modifier(Modifier::BOLD);
    let whisker = Style::default().fg(p.text_muted);
    vec![
        Line::styled(UPPER, mane),
        Line::from(vec![
            Span::styled("=", whisker),
            Span::styled("{", mane),
            Span::styled(eye.to_string(), eyes),
            Span::styled(nose.to_string(), star),
            Span::styled(eye.to_string(), eyes),
            Span::styled("}", mane),
            Span::styled("=", whisker),
        ]),
        Line::styled(LOWER, mane),
    ]
}

/// [`GLYPH`] in the palette, on `background`.
pub fn glyph(p: &Palette, background: Color) -> Vec<Span<'static>> {
    let mane = Style::default().fg(p.on_accent).bg(background);
    let star = mane.add_modifier(Modifier::BOLD);
    vec![
        Span::styled("{", mane),
        Span::styled("*", star),
        Span::styled("}", mane),
    ]
}

/// Which half of a twinkle `elapsed_ms` falls in. `every_ms` 0 never twinkles.
pub fn beat(elapsed_ms: u128, every_ms: usize) -> bool {
    every_ms > 0 && (elapsed_ms / every_ms as u128) % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::palette::palette;

    #[test]
    fn every_row_of_every_mood_is_as_wide_as_the_drawing() {
        for mood in [
            Mood::Idle,
            Mood::Working,
            Mood::Asking,
            Mood::Failed,
            Mood::Asleep,
        ] {
            for beat in [false, true] {
                for row in rows(mood, beat) {
                    assert_eq!(row.chars().count(), WIDTH as usize, "{mood:?}: {row:?}");
                    assert!(row.is_ascii(), "{row:?}");
                }
                let drawn: Vec<String> = lines(mood, beat, palette())
                    .iter()
                    .map(|line| line.spans.iter().map(|s| s.content.as_ref()).collect())
                    .collect();
                assert_eq!(drawn, rows(mood, beat).to_vec(), "{mood:?}");
            }
        }
    }

    #[test]
    fn the_face_says_what_goes_on() {
        assert_eq!(rows(Mood::Idle, false)[1], "={o*o}=");
        assert_eq!(rows(Mood::Working, true)[1], "={o+o}=");
        assert_eq!(rows(Mood::Working, false)[1], "={o*o}=");
        assert_eq!(rows(Mood::Asking, true)[1], "={o?o}=");
        assert_eq!(rows(Mood::Failed, false)[1], "={x*x}=");
        assert_eq!(rows(Mood::Asleep, false)[1], "={-*-}=");
        assert_eq!(rows(Mood::Idle, false)[0], " \\\\|// ");
        assert_eq!(rows(Mood::Idle, false)[2], " //|\\\\ ");
    }

    #[test]
    fn the_star_twinkles_on_its_beat_and_never_at_zero() {
        assert!(!beat(0, 400));
        assert!(beat(400, 400));
        assert!(!beat(800, 400));
        assert!(!beat(400, 0));
    }
}
