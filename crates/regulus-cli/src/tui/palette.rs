//! Regulus Midnight in the terminal: the same `assets/design/palette.json` the app's
//! tokens are generated from (docs/08-design-system.md), so both look alike.

use std::collections::HashMap;
use std::sync::OnceLock;

use ratatui::style::Color;
use serde::Deserialize;

const PALETTE_JSON: &str = include_str!("../../../../assets/design/palette.json");

#[derive(Deserialize)]
struct File {
    colors: HashMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub canvas: Color,
    pub sidebar: Color,
    pub surface: Color,
    pub selected: Color,
    pub border: Color,
    pub steel: Color,
    pub accent: Color,
    pub focus: Color,
    pub text: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub on_accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub info: Color,
}

fn hex(value: &str) -> Option<(u8, u8, u8)> {
    let value = value.strip_prefix('#')?;
    if value.len() != 6 {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&value[i..i + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// The palette, read once. A color missing from the file falls back to the terminal's own.
pub fn palette() -> &'static Palette {
    static PALETTE: OnceLock<Palette> = OnceLock::new();
    PALETTE.get_or_init(|| {
        let colors = serde_json::from_str::<File>(PALETTE_JSON)
            .map(|file| file.colors)
            .unwrap_or_default();
        let color = |name: &str| {
            colors
                .get(name)
                .and_then(|value| hex(value))
                .map_or(Color::Reset, |(r, g, b)| Color::Rgb(r, g, b))
        };
        Palette {
            canvas: color("canvas"),
            sidebar: color("sidebar"),
            surface: color("surface"),
            selected: color("selected"),
            border: color("border"),
            steel: color("steel"),
            accent: color("accent"),
            focus: color("focus"),
            text: color("text"),
            text_secondary: color("text-secondary"),
            text_muted: color("text-muted"),
            on_accent: color("on-accent"),
            success: color("success"),
            warning: color("warning"),
            danger: color("danger"),
            info: color("info"),
        }
    })
}

/// `from` moved towards `to` by `t` (0 = `from`, 1 = `to`). Colors that are not RGB do
/// not blend: the nearer end wins.
pub fn mix(from: Color, to: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (from, to) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let blend =
                |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
            Color::Rgb(blend(r1, r2), blend(g1, g2), blend(b1, b2))
        }
        _ if t < 0.5 => from,
        _ => to,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_color_comes_from_the_palette_file() {
        let p = palette();
        for color in [
            p.canvas,
            p.sidebar,
            p.surface,
            p.selected,
            p.border,
            p.steel,
            p.accent,
            p.focus,
            p.text,
            p.text_secondary,
            p.text_muted,
            p.on_accent,
            p.success,
            p.warning,
            p.danger,
            p.info,
        ] {
            assert!(matches!(color, Color::Rgb(..)), "{color:?}");
        }
    }

    #[test]
    fn mixing_moves_between_the_two_ends() {
        let black = Color::Rgb(0, 0, 0);
        let white = Color::Rgb(255, 255, 255);
        assert_eq!(mix(black, white, 0.0), black);
        assert_eq!(mix(black, white, 1.0), white);
        assert_eq!(mix(black, white, 0.5), Color::Rgb(128, 128, 128));
    }
}
