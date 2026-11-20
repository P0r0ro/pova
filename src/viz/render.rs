//! Drawing the visualizer frame into a ratatui buffer.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::symbols::Marker;
use ratatui::widgets::Widget;
use ratatui::widgets::canvas::{Canvas, Line};

use super::Frame;
use crate::config::VizMode;
use crate::theme::Theme;

const LOWER: [&str; 9] = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];

/// How many bars *per channel* the DSP should produce for this area.
pub fn bars_needed(width: u16, mode: VizMode, bar_width: u16, spacing: u16) -> usize {
    let bw = bar_width.max(1);
    let total = ((width + spacing) / (bw + spacing)).max(2) as usize;
    match mode {
        VizMode::Bars => (total / 2).max(1),
        VizMode::Mirror | VizMode::Mono => total,
        VizMode::Wave => 16, // unused, keep the DSP cheap
    }
}

pub struct VizWidget<'a> {
    pub frame: &'a Frame,
    pub mode: VizMode,
    pub theme: &'a Theme,
    pub bar_width: u16,
    pub spacing: u16,
}

impl VizWidget<'_> {
    /// Bar values in display order (left → right).
    fn values(&self) -> Vec<f64> {
        let b = &self.frame.bars;
        let n = b.len() / 2;
        if n == 0 {
            return Vec::new();
        }
        let (left, right) = b.split_at(n);
        match self.mode {
            // cava's stereo layout: left channel mirrored, lows in the centre.
            VizMode::Bars => left.iter().rev().chain(right.iter()).copied().collect(),
            _ => left.iter().zip(right).map(|(l, r)| (l + r) * 0.5).collect(),
        }
    }

    fn draw_bars(&self, area: Rect, buf: &mut Buffer, values: &[f64]) {
        let bw = self.bar_width.max(1);
        let step = bw + self.spacing;
        let used = (values.len() as u16).saturating_mul(step).saturating_sub(self.spacing);
        let x0 = area.x + area.width.saturating_sub(used) / 2;
        let h = area.height;
        let rows: Vec<Color> = (0..h)
            .map(|r| self.theme.viz_color(r as f32 / (h.max(2) - 1) as f32))
            .collect();
        for (i, v) in values.iter().enumerate() {
            let x = x0 + i as u16 * step;
            if x + bw > area.x + area.width {
                break;
            }
            let eighths = (v.clamp(0.0, 1.0) * h as f64 * 8.0).round() as u32;
            for row in 0..h {
                let fill = eighths.saturating_sub(row as u32 * 8).min(8) as usize;
                let y = area.y + h - 1 - row;
                let (sym, color) = if fill == 0 {
                    if row == 0 && !self.frame.active {
                        ("▁", self.theme.inactive_fg)
                    } else {
                        continue;
                    }
                } else {
                    (LOWER[fill], rows[row as usize])
                };
                for dx in 0..bw {
                    if let Some(cell) = buf.cell_mut((x + dx, y)) {
                        cell.set_symbol(sym).set_fg(color);
                    }
                }
            }
        }
    }

    fn draw_mirror(&self, area: Rect, buf: &mut Buffer, values: &[f64]) {
        let bw = self.bar_width.max(1);
        let step = bw + self.spacing;
        let used = (values.len() as u16).saturating_mul(step).saturating_sub(self.spacing);
        let x0 = area.x + area.width.saturating_sub(used) / 2;
        let up_rows = area.height / 2 + area.height % 2; // includes centre row
        let down_rows = area.height / 2;
        let center = area.y + up_rows - 1;
        for (i, v) in values.iter().enumerate() {
            let x = x0 + i as u16 * step;
            if x + bw > area.x + area.width {
                break;
            }
            let v = v.clamp(0.0, 1.0);
            // Upwards with eighth blocks.
            let eighths = (v * up_rows as f64 * 8.0).round() as u32;
            for row in 0..up_rows {
                let fill = eighths.saturating_sub(row as u32 * 8).min(8) as usize;
                if fill == 0 {
                    if row == 0 && !self.frame.active {
                        set(buf, x, center, bw, "▁", self.theme.inactive_fg);
                    }
                    continue;
                }
                let color = self.theme.viz_color(row as f32 / up_rows.max(1) as f32);
                set(buf, x, center - row, bw, LOWER[fill], color);
            }
            // Downwards (reflection) with half-block precision.
            let halves = (v * down_rows as f64 * 2.0).round() as u32;
            for row in 0..down_rows {
                let fill = halves.saturating_sub(row as u32 * 2).min(2);
                let sym = match fill {
                    0 => continue,
                    1 => "▀",
                    _ => "█",
                };
                let color = self.theme.viz_color(row as f32 / down_rows.max(1) as f32 * 0.8);
                set(buf, x, center + 1 + row, bw, sym, color);
            }
        }
    }

    fn draw_wave(&self, area: Rect, buf: &mut Buffer) {
        let wave = &self.frame.wave;
        let points = (area.width as usize * 2).max(2);
        if wave.len() < 2 {
            return;
        }
        let stride = wave.len() as f64 / points as f64;
        let sample = |i: usize| -> f64 {
            let idx = ((i as f64 * stride) as usize).min(wave.len() - 1);
            (wave[idx] as f64).clamp(-1.0, 1.0)
        };
        let theme = self.theme;
        Canvas::default()
            .marker(Marker::Braille)
            .x_bounds([0.0, points as f64 - 1.0])
            .y_bounds([-1.05, 1.05])
            .paint(|ctx| {
                for i in 1..points {
                    let (a, b) = (sample(i - 1), sample(i));
                    let color = theme.viz_color(a.abs().max(b.abs()) as f32);
                    ctx.draw(&Line {
                        x1: (i - 1) as f64,
                        y1: a,
                        x2: i as f64,
                        y2: b,
                        color,
                    });
                }
            })
            .render(area, buf);
    }
}

fn set(buf: &mut Buffer, x: u16, y: u16, w: u16, sym: &str, fg: Color) {
    for dx in 0..w {
        if let Some(cell) = buf.cell_mut((x + dx, y)) {
            cell.set_symbol(sym).set_style(Style::default().fg(fg));
        }
    }
}

impl Widget for VizWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        match self.mode {
            VizMode::Wave => self.draw_wave(area, buf),
            VizMode::Mirror => {
                let v = self.values();
                self.draw_mirror(area, buf, &v)
            }
            _ => {
                let v = self.values();
                self.draw_bars(area, buf, &v)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_counts() {
        assert_eq!(bars_needed(80, VizMode::Bars, 2, 1), 13);
        assert_eq!(bars_needed(80, VizMode::Mono, 2, 1), 27);
        assert_eq!(bars_needed(1, VizMode::Bars, 2, 1), 1);
    }

    #[test]
    fn draws_full_and_partial_blocks() {
        let theme = Theme::builtin_default(true);
        let frame = Frame {
            bars: vec![1.0, 0.5, 0.0, 0.0],
            wave: vec![],
            active: true,
        };
        let area = Rect::new(0, 0, 11, 2);
        let mut buf = Buffer::empty(area);
        VizWidget {
            frame: &frame,
            mode: VizMode::Mono,
            theme: &theme,
            bar_width: 1,
            spacing: 1,
        }
        .render(area, &mut buf);
        // Mono values: [0.5, 0.25] -> 8 eighths and 4 eighths over 2 rows.
        let row1: String = (0..11).map(|x| buf[(x, 1)].symbol().to_string()).collect();
        assert!(row1.contains('█') && row1.contains('▄'), "{row1:?}");
    }
}
