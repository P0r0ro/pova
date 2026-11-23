//! Contents of the player box tabs.

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, Wrap};
use ratatui_image::{Resize, StatefulImage};

use super::progress_spans;
use crate::app::{App, Hit, LibSection, ListView, LyricsState};
use crate::keymap::Action;
use crate::lyrics::{Lyrics, active_line};
use crate::model::{Item, format_time};
use crate::player::queue::Repeat;
use crate::theme::Theme;
use crate::ytm::SearchFilter;

const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];

fn spinner() -> &'static str {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    SPINNER[(ms / 100) as usize % SPINNER.len()]
}

fn centered_msg(f: &mut Frame, area: Rect, theme: &Theme, lines: Vec<Line<'static>>) {
    let h = lines.len() as u16;
    let y = area.y + area.height.saturating_sub(h) / 2;
    f.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true })
            .style(Style::new().fg(theme.inactive_fg)),
        Rect {
            y,
            height: h.min(area.height),
            ..area
        },
    );
}

// ───────────────────────────── now playing ─────────────────────────────

pub fn now_playing(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let Some(track) = app.current().cloned() else {
        let k = app.keymap.hint(Action::TabSearch);
        centered_msg(
            f,
            area,
            &t,
            vec![
                Line::from("♪ nothing playing"),
                Line::from(""),
                Line::from(format!("press {k} to search YouTube Music")),
            ],
        );
        return;
    };

    // Text block below the art: title, artist, album, gap, progress, time, status.
    let text_rows = 7u16;
    let art_rows = area.height.saturating_sub(text_rows + 1);
    // Terminal cells are ~2:1, so a square image is twice as wide as tall.
    let art_h = art_rows.min(area.width / 2);
    let mut y = area.y;
    if app.cfg.album_art && art_h >= 4 {
        let art_w = (art_h * 2).min(area.width);
        let art_area = Rect::new(area.x + (area.width - art_w) / 2, area.y, art_w, art_h);
        let mut drew = false;
        if let Some(proto) = app.art.as_mut()
            && !app.art_loading
            && let Some(size) = proto.size_for(Resize::Scale(None), art_area.into())
        {
            // Centre the fitted image inside the art area.
            let ax = art_area.x + (art_area.width.saturating_sub(size.width)) / 2;
            let ay = art_area.y + (art_area.height.saturating_sub(size.height)) / 2;
            let r = Rect::new(ax, ay, size.width.min(art_area.width), size.height.min(art_area.height));
            f.render_stateful_widget(StatefulImage::new().resize(Resize::Scale(None)), r, proto);
            drew = true;
        }
        if !drew {
            let block = Block::bordered()
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::new().fg(t.div_line));
            let inner = block.inner(art_area);
            f.render_widget(block, art_area);
            let icon = if app.art_loading { spinner() } else { "♪" };
            f.render_widget(
                Paragraph::new(icon)
                    .alignment(Alignment::Center)
                    .style(Style::new().fg(t.inactive_fg)),
                Rect {
                    y: inner.y + inner.height / 2,
                    height: 1,
                    ..inner
                },
            );
        }
        y += art_h + 1;
    } else {
        // No art: vertically centre the text block.
        y += area.height.saturating_sub(text_rows) / 2;
    }

    let line = |y: u16| Rect::new(area.x, y, area.width, 1);
    let bottom = area.y + area.height;
    let put = |f: &mut Frame, y: &mut u16, p: Paragraph| {
        if *y < bottom {
            f.render_widget(p.alignment(Alignment::Center), line(*y));
        }
        *y += 1;
    };
    let liked = if crate::app::is_liked(&app.likes, &track) {
        " ♥"
    } else {
        ""
    };
    put(
        f,
        &mut y,
        Paragraph::new(Line::from(vec![
            Span::styled(
                track.title.clone(),
                Style::new().fg(t.title).add_modifier(Modifier::BOLD),
            ),
            Span::styled(liked, Style::new().fg(t.hi_fg)),
        ])),
    );
    put(
        f,
        &mut y,
        Paragraph::new(Span::styled(track.artists.clone(), Style::new().fg(t.main_fg))),
    );
    put(
        f,
        &mut y,
        Paragraph::new(Span::styled(
            track.album.clone().unwrap_or_default(),
            Style::new().fg(t.graph_text),
        )),
    );
    y += 1;

    // Progress bar (clickable).
    if y < bottom {
        let w = area.width.saturating_sub(4);
        if w >= 2 {
            let frac = if app.player.duration > 0.0 {
                app.player.time / app.player.duration
            } else {
                0.0
            };
            let r = Rect::new(area.x + 2, y, w, 1);
            f.render_widget(Paragraph::new(Line::from(progress_spans(&t, w, frac))), r);
            app.hits.add(r, Hit::Progress);
        }
    }
    y += 1;
    if y < bottom {
        let r = Rect::new(area.x + 2, y, area.width.saturating_sub(4), 1);
        let status = if app.player.buffering || (app.player.idle && app.current().is_some() && !app.player.paused) {
            format!("{} loading", spinner())
        } else if app.player.paused {
            "paused".into()
        } else {
            String::new()
        };
        f.render_widget(
            Paragraph::new(format_time(app.player.time)).style(Style::new().fg(t.graph_text)),
            r,
        );
        f.render_widget(
            Paragraph::new(status)
                .alignment(Alignment::Center)
                .style(Style::new().fg(t.inactive_fg)),
            r,
        );
        f.render_widget(
            Paragraph::new(format_time(app.player.duration))
                .alignment(Alignment::Right)
                .style(Style::new().fg(t.graph_text)),
            r,
        );
    }
    y += 1;
    if y < bottom {
        let dim = Style::new().fg(t.inactive_fg);
        let lit = Style::new().fg(t.accent);
        let vol = if app.player.muted {
            "muted".into()
        } else {
            format!("vol {:.0}%", app.player.volume)
        };
        let pos = app
            .queue
            .current
            .map(|c| format!("{}/{}", c + 1, app.queue.len()))
            .unwrap_or_default();
        let line_spans = vec![
            Span::styled(vol, Style::new().fg(t.graph_text)),
            Span::raw("   "),
            Span::styled("⇄ shuffle", if app.queue.shuffle { lit } else { dim }),
            Span::raw("   "),
            Span::styled(
                format!("↻ {}", app.queue.repeat.label()),
                if app.queue.repeat != Repeat::Off { lit } else { dim },
            ),
            Span::raw("   "),
            Span::styled(pos, Style::new().fg(t.graph_text)),
        ];
        f.render_widget(
            Paragraph::new(Line::from(line_spans)).alignment(Alignment::Center),
            line(y),
        );
    }
}

// ───────────────────────────── lists ─────────────────────────────

fn table_style(t: &Theme) -> Style {
    Style::new()
        .bg(t.selected_bg)
        .fg(t.selected_fg)
        .add_modifier(Modifier::BOLD)
}

pub fn queue(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    if app.queue.is_empty() {
        centered_msg(
            f,
            area,
            &t,
            vec![
                Line::from("queue is empty"),
                Line::from(""),
                Line::from(format!(
                    "{} search · {} add · {} play next",
                    app.keymap.hint(Action::TabSearch),
                    app.keymap.hint(Action::Append),
                    app.keymap.hint(Action::PlayNext)
                )),
            ],
        );
        return;
    }
    let cur = app.queue.current;
    let rows: Vec<Row> = app
        .queue
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let tr = &e.track;
            let is_cur = Some(i) == cur;
            let played = cur.is_some_and(|c| i < c);
            let style = if is_cur {
                Style::new().fg(t.accent).add_modifier(Modifier::BOLD)
            } else if played {
                Style::new().fg(t.inactive_fg)
            } else {
                Style::new().fg(t.main_fg)
            };
            let marker = if is_cur {
                if app.player.paused { "⏸" } else { "▶" }
            } else {
                ""
            };
            let heart = if crate::app::is_liked(&app.likes, tr) {
                "♥"
            } else {
                ""
            };
            Row::new(vec![
                Cell::from(marker),
                Cell::from(format!("{}", i + 1)).style(Style::new().fg(t.graph_text)),
                Cell::from(heart).style(Style::new().fg(t.hi_fg)),
                Cell::from(tr.title.clone()),
                Cell::from(tr.artists.clone()).style(if played { style } else { Style::new().fg(t.graph_text) }),
                Cell::from(tr.duration.map(format_time).unwrap_or_default()),
            ])
            .style(style)
        })
        .collect();
    let num_w = (app.queue.len().to_string().len() as u16).max(2);
    let header = Row::new(vec!["", "#", "", "title", "artist", "time"]).style(Style::new().fg(t.hi_fg));
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Length(num_w),
            Constraint::Length(1),
            Constraint::Fill(5),
            Constraint::Fill(3),
            Constraint::Length(7),
        ],
    )
    .header(header)
    .column_spacing(1)
    .row_highlight_style(table_style(&t));
    if app.queue_state.selected().is_none() {
        app.queue_state.select(cur);
    }
    f.render_stateful_widget(table, area, &mut app.queue_state);
    let list_area = Rect {
        y: area.y + 1,
        height: area.height.saturating_sub(1),
        ..area
    };
    app.hits.list = Some((list_area, app.queue_state.offset()));
    app.hits.add(list_area, Hit::List);
}

fn item_rows<'a>(
    t: &Theme,
    items: &'a [Item],
    playing: Option<&str>,
    likes: &std::collections::HashMap<String, bool>,
) -> Vec<Row<'a>> {
    items
        .iter()
        .map(|item| match item {
            Item::Track(tr) => {
                let is_playing = playing == Some(tr.video_id.as_str());
                let style = if is_playing {
                    Style::new().fg(t.accent)
                } else {
                    Style::new().fg(t.main_fg)
                };
                Row::new(vec![
                    Cell::from(if is_playing {
                        "▶"
                    } else if crate::app::is_liked(likes, tr) {
                        "♥"
                    } else {
                        "♪"
                    })
                    .style(Style::new().fg(t.hi_fg)),
                    Cell::from(tr.title.as_str()),
                    Cell::from(tr.artists.as_str()).style(Style::new().fg(t.graph_text)),
                    Cell::from(tr.duration.map(format_time).unwrap_or_default()),
                ])
                .style(style)
            }
            Item::Album(a) => Row::new(vec![
                Cell::from("◉").style(Style::new().fg(t.hi_fg)),
                Cell::from(a.title.as_str()),
                Cell::from(a.subtitle.as_str()).style(Style::new().fg(t.graph_text)),
                Cell::from("album").style(Style::new().fg(t.inactive_fg)),
            ]),
            Item::Artist(a) => Row::new(vec![
                Cell::from("☺").style(Style::new().fg(t.hi_fg)),
                Cell::from(a.name.as_str()),
                Cell::from(a.subtitle.as_str()).style(Style::new().fg(t.graph_text)),
                Cell::from("artist").style(Style::new().fg(t.inactive_fg)),
            ]),
            Item::Playlist(p) => Row::new(vec![
                Cell::from("≡").style(Style::new().fg(t.hi_fg)),
                Cell::from(p.title.as_str()),
                Cell::from(p.subtitle.as_str()).style(Style::new().fg(t.graph_text)),
                Cell::from("list").style(Style::new().fg(t.inactive_fg)),
            ]),
            Item::Header(h) => Row::new(vec![
                Cell::from(""),
                Cell::from(h.as_str()).style(
                    Style::new()
                        .fg(t.title)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                ),
            ]),
        })
        .collect()
}

/// Breadcrumb + table for a stack of list views.
fn browse(f: &mut Frame, app: &mut App, area: Rect, library: bool) {
    let t = app.theme.clone();
    let playing = app.current().map(|tr| tr.video_id.clone());
    let back = app.keymap.hint(Action::Menu);
    let likes = &app.likes;
    let stack: &mut Vec<ListView> = if library {
        &mut app.library.stack
    } else {
        &mut app.search.stack
    };
    if stack.is_empty() {
        return;
    }
    let depth = stack.len();
    let lv = stack.last_mut().unwrap();

    let [crumb, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let mut spans = vec![Span::styled(
        if lv.title.is_empty() {
            "…".to_string()
        } else {
            lv.title.clone()
        },
        Style::new().fg(t.title).add_modifier(Modifier::BOLD),
    )];
    if lv.loading {
        spans.push(Span::styled(format!("  {}", spinner()), Style::new().fg(t.accent)));
    } else if !lv.items.is_empty() {
        let n = lv.items.iter().filter(|i| !matches!(i, Item::Header(_))).count();
        spans.push(Span::styled(format!("  {n} items"), Style::new().fg(t.inactive_fg)));
    }
    if depth > 1 {
        spans.push(Span::styled(format!("  ‹{back} back›"), Style::new().fg(t.inactive_fg)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), crumb);

    if let Some(err) = &lv.error {
        // Wrap here so the full message shows (centered_msg sizes by line count).
        let width = body.width.saturating_sub(4).clamp(20, 90) as usize;
        let lines = crate::ui::wrap_text(err, width)
            .into_iter()
            .map(|l| Line::from(Span::styled(l, Style::new().fg(crate::ui::ERROR_RED))))
            .collect();
        centered_msg(f, body, &t, lines);
        return;
    }
    if lv.loading && lv.items.is_empty() {
        centered_msg(f, body, &t, vec![Line::from(format!("{} loading…", spinner()))]);
        return;
    }
    let rows = item_rows(&t, &lv.items, playing.as_deref(), likes);
    let table = Table::new(
        rows,
        [
            Constraint::Length(1),
            Constraint::Fill(5),
            Constraint::Fill(3),
            Constraint::Length(7),
        ],
    )
    .column_spacing(1)
    .row_highlight_style(table_style(&t));
    f.render_stateful_widget(table, body, &mut lv.state);
    let offset = lv.state.offset();
    app.hits.list = Some((body, offset));
    app.hits.add(body, Hit::List);
}

fn chips<T: Copy + PartialEq>(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    items: &[(T, &str)],
    active: T,
    hit: impl Fn(T) -> Hit,
) {
    let t = app.theme.clone();
    let mut spans = Vec::new();
    let mut x = area.x;
    for (val, label) in items {
        let text = format!(" {label} ");
        let w = Span::raw(text.as_str()).width() as u16;
        let style = if *val == active {
            Style::new()
                .bg(t.selected_bg)
                .fg(t.selected_fg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.graph_text)
        };
        if x + w <= area.x + area.width {
            app.hits.add(Rect::new(x, area.y, w, 1), hit(*val));
        }
        x += w + 1;
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
    }
    let hint = format!(
        "{}/{}",
        app.keymap.hint(Action::SubPrev),
        app.keymap.hint(Action::SubNext)
    );
    spans.push(Span::styled(hint, Style::new().fg(t.inactive_fg)));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub fn search(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let [input, filters, gap, body] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(area);

    let prompt_style = if app.search.editing {
        Style::new().fg(t.hi_fg)
    } else {
        Style::new().fg(t.graph_text)
    };
    let text = if app.search.input.is_empty() && !app.search.editing {
        Span::styled(
            format!("press {} to search…", app.keymap.hint(Action::TabSearch)),
            Style::new().fg(t.inactive_fg),
        )
    } else {
        Span::styled(app.search.input.clone(), Style::new().fg(t.title))
    };
    let mut line = vec![Span::styled("⌕ ", prompt_style), text];
    // Inline completion from the top suggestion, dimmed (→ accepts it).
    if let Some(ghost) = app.search.ghost() {
        line.push(Span::styled(ghost.to_string(), Style::new().fg(t.inactive_fg)));
    }
    f.render_widget(Paragraph::new(Line::from(line)), input);
    if app.search.editing {
        let before: String = app.search.input.chars().take(app.search.cursor).collect();
        let cx = input.x + 2 + Span::raw(before.as_str()).width() as u16;
        f.set_cursor_position((cx.min(input.x + input.width.saturating_sub(1)), input.y));
    }
    // Divider under the input, btop style.
    f.render_widget(
        Paragraph::new("─".repeat(gap.width as usize)).style(Style::new().fg(t.div_line)),
        gap,
    );

    let items: Vec<(SearchFilter, &str)> = SearchFilter::ALL.iter().map(|f| (*f, f.label())).collect();
    let active = app.search.filter;
    chips(f, app, filters, &items, active, Hit::SearchFilter);

    if app.search.stack.is_empty() {
        centered_msg(
            f,
            body,
            &t,
            vec![
                Line::from("type and press enter"),
                Line::from("tab switches the filter while typing"),
            ],
        );
    } else {
        browse(f, app, body, false);
    }
    suggestions_dropdown(
        f,
        app,
        Rect {
            y: input.y + 1,
            height: area.height.saturating_sub(1),
            ..area
        },
    );
}

/// Autocomplete list under the search box, drawn over the results.
fn suggestions_dropdown(f: &mut Frame, app: &mut App, below: Rect) {
    let t = app.theme.clone();
    let list = app.search.visible_suggestions().to_vec();
    if list.is_empty() || below.height < 3 {
        return;
    }
    let rows = (list.len() as u16).min(below.height - 2);
    let r = Rect {
        height: rows + 2,
        ..below
    };
    f.render_widget(ratatui::widgets::Clear, r);
    let block = Block::bordered()
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(Style::new().fg(t.hi_fg))
        .style(Style::new().bg(t.main_bg))
        .title_bottom(
            Line::from(Span::styled(
                "┐↑↓ select · → complete · enter search┌",
                Style::new().fg(t.inactive_fg),
            ))
            .alignment(Alignment::Right),
        );
    let inner = block.inner(r);
    f.render_widget(block, r);
    let typed = app.search.input.trim();
    for (i, sugg) in list.iter().take(rows as usize).enumerate() {
        let y = inner.y + i as u16;
        let row = Rect { y, height: 1, ..inner };
        let selected = app.search.suggest_sel == Some(i);
        let base = if selected {
            Style::new().bg(t.selected_bg).fg(t.selected_fg)
        } else {
            Style::new().fg(t.main_fg)
        };
        // Like YTM: the typed part plain, the completion bold.
        let split = crate::app::case_insensitive_prefix_len(&sugg.text, typed).unwrap_or(0);
        let (head, tail) = sugg.text.split_at(split);
        let icon = if sugg.history { "↺ " } else { "⌕ " };
        let spans = vec![
            Span::styled(
                format!(" {icon}"),
                base.fg(if selected { t.selected_fg } else { t.hi_fg }),
            ),
            Span::styled(head.to_string(), base),
            Span::styled(tail.to_string(), base.add_modifier(Modifier::BOLD)),
        ];
        f.render_widget(Paragraph::new(Line::from(spans)).style(base), row);
        app.hits.add(row, Hit::Suggestion(i));
    }
}

pub fn library(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let [sections, gap, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let items: Vec<(LibSection, &str)> = LibSection::ALL.iter().map(|s| (*s, s.label())).collect();
    let active = app.library.section;
    chips(f, app, sections, &items, active, Hit::LibSection);
    f.render_widget(
        Paragraph::new("─".repeat(gap.width as usize)).style(Style::new().fg(t.div_line)),
        gap,
    );
    if !app.logged_in {
        let path = app.paths.cookie_file();
        centered_msg(
            f,
            body,
            &t,
            vec![
                Line::from(Span::styled(
                    "not signed in",
                    Style::new().fg(t.title).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from("1. open music.youtube.com in your browser (signed in)"),
                Line::from("2. devtools → Network → any request to music.youtube.com"),
                Line::from("3. copy the full Cookie request header value into"),
                Line::from(Span::styled(path.display().to_string(), Style::new().fg(t.accent))),
                Line::from("4. restart pova"),
            ],
        );
        return;
    }
    browse(f, app, body, true);
}

// ───────────────────────────── lyrics ─────────────────────────────

pub fn lyrics(f: &mut Frame, app: &mut App, area: Rect) {
    let t = app.theme.clone();
    let Some(track) = app.current().cloned() else {
        centered_msg(f, area, &t, vec![Line::from("nothing playing")]);
        return;
    };
    let [head, gap, body] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1), Constraint::Min(0)]).areas(area);
    let source = match &app.lyrics {
        LyricsState::Found { source, lyrics, .. } => {
            format!(
                "{source}{}",
                if matches!(lyrics, Lyrics::Synced(_)) {
                    " · synced"
                } else {
                    ""
                }
            )
        }
        _ => String::new(),
    };
    let source_w = if source.is_empty() {
        0
    } else {
        Span::raw(source.as_str()).width() as u16 + 2
    };
    let [title_area, source_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(source_w.min(head.width / 2))]).areas(head);
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                track.title.clone(),
                Style::new().fg(t.title).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" — {}", track.artists), Style::new().fg(t.graph_text)),
        ])),
        title_area,
    );
    f.render_widget(
        Paragraph::new(source)
            .alignment(Alignment::Right)
            .style(Style::new().fg(t.inactive_fg)),
        source_area,
    );
    f.render_widget(
        Paragraph::new("─".repeat(gap.width as usize)).style(Style::new().fg(t.div_line)),
        gap,
    );

    match &app.lyrics {
        LyricsState::None => {}
        LyricsState::Loading { .. } => centered_msg(
            f,
            body,
            &t,
            vec![Line::from(format!("{} looking for lyrics…", spinner()))],
        ),
        LyricsState::Missing => no_lyrics(f, app, body),
        LyricsState::Found {
            lyrics: Lyrics::Plain(text),
            ..
        } => {
            let scroll = app.lyrics_scroll.unwrap_or(0);
            let p = Paragraph::new(text.clone())
                .wrap(Wrap { trim: false })
                .alignment(Alignment::Center)
                .style(Style::new().fg(t.main_fg))
                .scroll((scroll, 0));
            f.render_widget(p, body);
        }
        LyricsState::Found {
            lyrics: Lyrics::Synced(lines),
            ..
        } => {
            // Small lead so the highlight lands as the line is sung.
            let active = active_line(lines, app.player.time + 0.25);
            let w = body.width.max(1) as usize;
            let rows_of = |s: &str| (Span::raw(s).width().max(1)).div_ceil(w) as u16;
            let mut text = Vec::with_capacity(lines.len());
            let mut active_row = 0u16;
            let mut row = 0u16;
            for (i, (_, l)) in lines.iter().enumerate() {
                let content = if l.is_empty() { "♪".to_string() } else { l.clone() };
                let style = match active {
                    Some(a) if a == i => {
                        active_row = row;
                        Style::new().fg(t.hi_fg).add_modifier(Modifier::BOLD)
                    }
                    Some(a) if i < a => Style::new().fg(t.inactive_fg),
                    _ => Style::new().fg(t.main_fg),
                };
                row += rows_of(&content);
                text.push(Line::styled(content, style));
            }
            let auto = active_row.saturating_sub(body.height / 2);
            app.lyrics_auto = auto;
            let scroll = app.lyrics_scroll.unwrap_or(auto);
            f.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .alignment(Alignment::Center)
                    .scroll((scroll, 0)),
                body,
            );
        }
    }
}

/// The sad pixel cat (the pova mascot) above "no lyrics".
fn no_lyrics(f: &mut Frame, app: &mut App, body: Rect) {
    let t = app.theme.clone();
    let msg = "no lyrics available for this track";
    // Cat image: up to 14 rows, leaving room for the message.
    let img_h = body.height.saturating_sub(3).min(14);
    let mut drew = false;
    if img_h >= 4
        && let Some(cat) = app.cat_art.as_mut()
    {
        let max = Rect { height: img_h, ..body };
        if let Some(size) = cat.size_for(Resize::Scale(None), max.into()) {
            let total_h = size.height + 2;
            let y0 = body.y + body.height.saturating_sub(total_h) / 2;
            let x0 = body.x + body.width.saturating_sub(size.width) / 2;
            let r = Rect::new(x0, y0, size.width.min(body.width), size.height.min(img_h));
            f.render_stateful_widget(StatefulImage::new().resize(Resize::Scale(None)), r, cat);
            f.render_widget(
                Paragraph::new(msg)
                    .alignment(Alignment::Center)
                    .style(Style::new().fg(t.inactive_fg)),
                Rect {
                    y: r.y + r.height + 1,
                    height: 1,
                    ..body
                },
            );
            drew = true;
        }
    }
    if !drew {
        centered_msg(f, body, &t, vec![Line::from(msg)]);
    }
}
