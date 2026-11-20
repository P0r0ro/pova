//! Modal overlays: help, theme picker, btop-like menu.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Row, Table};

use crate::app::{App, MENU_ITEMS, Overlay};
use crate::keymap::Action;

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn frame<'a>(t: &crate::theme::Theme, title: &'a str) -> Block<'a> {
    Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(t.hi_fg))
        .style(Style::new().bg(t.main_bg).fg(t.main_fg))
        .title(Line::from(vec![
            Span::styled("─┐", Style::new().fg(t.hi_fg)),
            Span::styled(title, Style::new().fg(t.title).add_modifier(Modifier::BOLD)),
            Span::styled("┌", Style::new().fg(t.hi_fg)),
        ]))
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let t = app.theme.clone();
    let sel = Style::new()
        .bg(t.selected_bg)
        .fg(t.selected_fg)
        .add_modifier(Modifier::BOLD);
    let Some(overlay) = app.overlay.as_mut() else { return };
    match overlay {
        Overlay::Help { scroll } => {
            let r = centered(area, 72, area.height.saturating_sub(4).max(10));
            f.render_widget(Clear, r);
            let block = frame(&t, "help").title_bottom(
                Line::from(Span::styled("┐any key closes┌", Style::new().fg(t.inactive_fg)))
                    .alignment(Alignment::Right),
            );
            let inner = block.inner(r);
            f.render_widget(block, r);
            let rows: Vec<Row> = Action::ALL
                .iter()
                .map(|a| {
                    let keys = app.keymap.keys_for(*a).join("  ");
                    Row::new(vec![
                        Cell::from(keys).style(Style::new().fg(t.hi_fg)),
                        Cell::from(a.description()),
                        Cell::from(a.name()).style(Style::new().fg(t.inactive_fg)),
                    ])
                })
                .collect();
            let max = (rows.len() as u16).saturating_sub(inner.height);
            *scroll = (*scroll).min(max);
            let rows: Vec<Row> = rows.into_iter().skip(*scroll as usize).collect();
            let table = Table::new(
                rows,
                [Constraint::Length(20), Constraint::Fill(1), Constraint::Length(18)],
            )
            .column_spacing(2);
            f.render_widget(
                table,
                Rect {
                    x: inner.x + 1,
                    width: inner.width.saturating_sub(2),
                    ..inner
                },
            );
        }
        Overlay::Themes { names, state, .. } => {
            let h = (names.len() as u16 + 2).min(area.height.saturating_sub(2)).max(5);
            let r = centered(area, 40, h);
            f.render_widget(Clear, r);
            let block = frame(&t, "themes").title_bottom(
                Line::from(Span::styled(
                    "┐enter keep · esc revert┌",
                    Style::new().fg(t.inactive_fg),
                ))
                .alignment(Alignment::Right),
            );
            let inner = block.inner(r);
            f.render_widget(block, r);
            let rows: Vec<Row> = names
                .iter()
                .map(|(n, p)| {
                    let origin = if p.is_none() { "built-in" } else { "" };
                    Row::new(vec![
                        Cell::from(n.as_str()),
                        Cell::from(origin).style(Style::new().fg(t.inactive_fg)),
                    ])
                })
                .collect();
            let table = Table::new(rows, [Constraint::Fill(1), Constraint::Length(9)]).row_highlight_style(sel);
            f.render_stateful_widget(table, inner, state);
        }
        Overlay::Menu { state } => {
            let r = centered(area, 30, MENU_ITEMS.len() as u16 + 4);
            f.render_widget(Clear, r);
            let block = frame(&t, "pova");
            let inner = block.inner(r);
            f.render_widget(block, r);
            let rows: Vec<Row> = MENU_ITEMS.iter().map(|m| Row::new(vec![Cell::from(*m)])).collect();
            let table = Table::new(rows, [Constraint::Fill(1)]).row_highlight_style(sel);
            let inner = Rect {
                x: inner.x + 1,
                y: inner.y + 1,
                width: inner.width.saturating_sub(2),
                height: inner.height.saturating_sub(1),
            };
            f.render_stateful_widget(table, inner, state);
        }
    }
}
