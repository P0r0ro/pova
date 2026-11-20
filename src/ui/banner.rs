//! The pova logo as terminal art (shown in the Esc menu, like btop's banner).
//!
//! Recreates `assets/logo.png`: a waveform icon with a violet→pink gradient,
//! a divider, and white pixel letters, inside a gradient frame. Pixels are
//! drawn with half blocks (▀ ▄ █), which makes them roughly square.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Logo colours, sampled from assets/logo.png.
const VIOLET: (u8, u8, u8) = (0x6b, 0x1f, 0xfc);
const PURPLE: (u8, u8, u8) = (0xa8, 0x1e, 0xf1);
const PINK: (u8, u8, u8) = (0xff, 0x00, 0x7f);
const WHITE: Color = Color::Rgb(0xf4, 0xf2, 0xff);

/// 5×7 lowercase glyphs (x-height 5 + 2 rows of descender).
const GLYPHS: [(char, [&str; 7]); 4] = [
    ('p', ["####.", "#...#", "#...#", "#...#", "####.", "#....", "#...."]),
    ('o', [".###.", "#...#", "#...#", "#...#", ".###.", ".....", "....."]),
    ('v', ["#...#", "#...#", "#...#", ".#.#.", "..#..", ".....", "....."]),
    ('a', [".###.", "....#", ".####", "#...#", ".####", ".....", "....."]),
];

/// Waveform bar heights (pixels), like the logo's five bars.
const BARS: [usize; 5] = [3, 5, 7, 5, 3];
const HEIGHT: usize = 8;

/// Gradient across the logo: violet → purple → pink.
pub fn gradient(t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let lerp = |a: (u8, u8, u8), b: (u8, u8, u8), t: f32| {
        let f = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
        Color::Rgb(f(a.0, b.0), f(a.1, b.1), f(a.2, b.2))
    };
    if t < 0.5 {
        lerp(VIOLET, PURPLE, t * 2.0)
    } else {
        lerp(PURPLE, PINK, (t - 0.5) * 2.0)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Ink {
    None,
    Wave(f32),
    Divider,
    Letter,
}

/// The logo as a pixel grid (scale 1): `HEIGHT` rows.
fn bitmap() -> Vec<Vec<Ink>> {
    let mut cols: Vec<[Ink; HEIGHT]> = Vec::new();
    let blank = [Ink::None; HEIGHT];
    // Waveform, centred on the letters' x-height (rows 1..=5, centre 3).
    for (i, h) in BARS.iter().enumerate() {
        let top = 3 - h / 2;
        let mut col = blank;
        for cell in col.iter_mut().skip(top).take(*h) {
            *cell = Ink::Wave(i as f32 / (BARS.len() - 1) as f32);
        }
        cols.push(col);
        if i + 1 < BARS.len() {
            cols.push(blank);
        }
    }
    cols.extend([blank, blank]);
    cols.push([Ink::Divider; HEIGHT]);
    cols.extend([blank, blank]);
    for (gi, (_, rows)) in GLYPHS.iter().enumerate() {
        for x in 0..5 {
            let mut col = blank;
            for (y, row) in rows.iter().enumerate() {
                if row.as_bytes()[x] == b'#' {
                    col[y + 1] = Ink::Letter; // letters start one row down
                }
            }
            cols.push(col);
        }
        if gi + 1 < GLYPHS.len() {
            cols.push(blank);
        }
    }
    // Transpose to rows.
    (0..HEIGHT).map(|y| cols.iter().map(|c| c[y]).collect()).collect()
}

fn ink_color(ink: Ink) -> Color {
    match ink {
        Ink::Wave(t) => gradient(t),
        Ink::Divider => gradient(0.5),
        Ink::Letter => WHITE,
        Ink::None => Color::Reset,
    }
}

/// Width in columns of [`lines`] for a given scale (including the frame).
pub fn width(scale: usize) -> u16 {
    (bitmap()[0].len() * scale + 2 * PAD * scale + 2) as u16
}

const PAD: usize = 2;

/// Render the framed logo. `scale` 1 = compact (6 rows), 2 = large (10 rows).
pub fn lines(scale: usize) -> Vec<Line<'static>> {
    let scale = scale.max(1);
    let bm = bitmap();
    // Scale up in both directions.
    let px: Vec<Vec<Ink>> = bm
        .iter()
        .flat_map(|row| {
            let wide: Vec<Ink> = row.iter().flat_map(|p| std::iter::repeat_n(*p, scale)).collect();
            std::iter::repeat_n(wide, scale)
        })
        .collect();
    let inner_w = px[0].len() + 2 * PAD * scale;
    let total_w = inner_w + 2;
    let border = |x: usize| Style::new().fg(gradient(x as f32 / (total_w - 1) as f32));

    let mut out = Vec::new();
    // Top border with chamfered corners, like the logo's frame.
    let mut top = vec![Span::styled("╭", border(0))];
    top.extend((1..total_w - 1).map(|x| Span::styled("─", border(x))));
    top.push(Span::styled("╮", border(total_w - 1)));
    out.push(Line::from(top));

    for pair in px.chunks(2) {
        let (upper, lower) = (&pair[0], pair.get(1));
        let mut spans = vec![Span::styled("│", border(0)), Span::raw(" ".repeat(PAD * scale))];
        for x in 0..upper.len() {
            let a = upper[x];
            let b = lower.map_or(Ink::None, |l| l[x]);
            let (sym, ink) = match (a != Ink::None, b != Ink::None) {
                (true, true) => ("█", a),
                (true, false) => ("▀", a),
                (false, true) => ("▄", b),
                (false, false) => (" ", Ink::None),
            };
            let mut style = Style::new().fg(ink_color(ink));
            if ink == Ink::Letter {
                style = style.add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(sym, style));
        }
        spans.push(Span::raw(" ".repeat(PAD * scale)));
        spans.push(Span::styled("│", border(total_w - 1)));
        out.push(Line::from(spans));
    }

    let mut bottom = vec![Span::styled("╰", border(0))];
    bottom.extend((1..total_w - 1).map(|x| Span::styled("─", border(x))));
    bottom.push(Span::styled("╯", border(total_w - 1)));
    out.push(Line::from(bottom));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn banner_is_rectangular_and_sized() {
        for scale in [1, 2] {
            let l = lines(scale);
            assert_eq!(l.len(), HEIGHT * scale / 2 + 2);
            let w = width(scale) as usize;
            for row in text(&l) {
                assert_eq!(row.chars().count(), w, "{row:?}");
            }
        }
        assert!(width(1) <= 50 && width(2) <= 90);
    }

    #[test]
    fn banner_has_wave_divider_and_letters() {
        let rows = text(&lines(1));
        let body = rows[1..rows.len() - 1].join("\n");
        assert!(body.contains('█') && body.contains('▀') && body.contains('▄'));
        // The waveform's first bar is violet and the last is pink.
        assert_eq!(gradient(0.0), Color::Rgb(0x6b, 0x1f, 0xfc));
        assert_eq!(gradient(1.0), Color::Rgb(0xff, 0x00, 0x7f));
    }
}
