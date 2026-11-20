//! pova — a btop-style YouTube Music player with a built-in cava visualizer.

mod app;
mod art;
mod config;
mod keymap;
mod lyrics;
mod model;
mod player;
mod theme;
mod ui;
mod viz;
mod ytm;

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, EventStream,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui_image::picker::Picker;
use ratatui_image::thread::{ResizeRequest, ThreadProtocol};
use tokio::sync::mpsc::unbounded_channel;

use crate::app::{App, Channels};
use crate::config::{Config, Paths, State};
use crate::keymap::Keymap;
use crate::player::mpv::{self, MpvConfig, PlayerCmd};

#[derive(Parser, Debug)]
#[command(version, about = "btop-style YouTube Music player with a cava visualizer")]
struct Cli {
    /// Theme name (btop .theme file stem), overrides config/state.
    #[arg(short, long)]
    theme: Option<String>,
    /// Search for songs and start playing the first result.
    #[arg(short, long, value_name = "QUERY")]
    play: Option<String>,
    /// Disable album art.
    #[arg(long)]
    no_art: bool,
    /// Use an alternative config file.
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// List available themes and exit.
    #[arg(long)]
    list_themes: bool,
    /// Log verbosity written to ~/.cache/pova/pova.log (error, warn, info, debug, trace).
    #[arg(long, default_value = "info")]
    log_level: tracing::Level,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let paths = Paths::new()?;

    if cli.list_themes {
        for (name, path) in theme::list_themes(&paths.config_dir) {
            match path {
                Some(p) => println!("{name:<28} {}", p.display()),
                None => println!("{name:<28} (built-in)"),
            }
        }
        return Ok(());
    }

    // Logs go to a file only; the TUI owns the terminal.
    let appender = tracing_appender::rolling::never(&paths.cache_dir, "pova.log");
    let (writer, _log_guard) = tracing_appender::non_blocking(appender);
    // Verbose levels apply to pova only; dependencies stay at warn.
    let filter = tracing_subscriber::EnvFilter::new(format!("warn,pova={}", cli.log_level));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();
    tracing::info!("pova {} starting", env!("CARGO_PKG_VERSION"));

    let mut cfg = Config::load(cli.config.as_deref().unwrap_or(&paths.config_file()))?;
    if cli.no_art {
        cfg.album_art = false;
    }
    let mut state = State::load(&paths.state_file());
    if let Some(t) = &cli.theme {
        state.theme = Some(t.clone());
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(run(cli, cfg, paths, state))
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn setup_terminal() -> anyhow::Result<(Term, bool)> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    // Kitty keyboard protocol: unambiguous Esc / shift+arrows.
    let enhanced = matches!(crossterm::terminal::supports_keyboard_enhancement(), Ok(true));
    if enhanced {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let mut term = Terminal::new(CrosstermBackend::new(out))?;
    term.hide_cursor()?;
    term.clear()?;
    Ok((term, enhanced))
}

fn restore_terminal(enhanced: bool) {
    let mut out = io::stdout();
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = disable_raw_mode();
    let _ = out.flush();
}

/// Whether the terminal reliably answers ratatui-image's capability query.
/// If a terminal never answers, the query thread stays blocked on stdin and
/// swallows the user's next key press, so only query known-good terminals.
fn terminal_answers_queries() -> bool {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    if !env("TMUX").is_empty() || env("TERM").starts_with("screen") {
        return false;
    }
    let term = env("TERM");
    let program = env("TERM_PROGRAM").to_lowercase();
    !env("KITTY_WINDOW_ID").is_empty()
        || term.contains("kitty")
        || term.contains("ghostty")
        || term.starts_with("foot")
        || !env("WEZTERM_EXECUTABLE").is_empty()
        || !env("KONSOLE_VERSION").is_empty()
        || ["wezterm", "ghostty", "iterm.app"].contains(&program.as_str())
}

/// Pick the image protocol before the event stream starts reading stdin.
fn make_picker(protocol: &str) -> Picker {
    use ratatui_image::picker::ProtocolType;
    let forced = match protocol.to_ascii_lowercase().as_str() {
        "kitty" => Some(ProtocolType::Kitty),
        "sixel" => Some(ProtocolType::Sixel),
        "iterm2" => Some(ProtocolType::Iterm2),
        "halfblocks" => Some(ProtocolType::Halfblocks),
        _ => None,
    };
    let mut picker = if forced != Some(ProtocolType::Halfblocks) && terminal_answers_queries() {
        match Picker::from_query_stdio() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("graphics query failed ({e}); using halfblocks");
                Picker::halfblocks()
            }
        }
    } else {
        Picker::halfblocks()
    };
    if let Some(p) = forced {
        picker.set_protocol_type(p);
    }
    tracing::info!("image protocol: {:?}", picker.protocol_type());
    picker
}

/// Discard pending terminal input (late query responses) for up to `quiet`.
fn drain_stdin(quiet: Duration) {
    while let Ok(true) = crossterm::event::poll(quiet) {
        if crossterm::event::read().is_err() {
            break;
        }
    }
}

async fn run(cli: Cli, cfg: Config, paths: Paths, state: State) -> anyhow::Result<()> {
    let (keymap, key_warnings) = Keymap::new(&cfg.keys);

    // ── visualizer threads ──
    let mut viz = viz::spawn(&cfg.viz);

    // ── actors ──
    let (player_tx, player_cmd_rx) = unbounded_channel();
    let (player_ev_tx, mut player_rx) = unbounded_channel();
    let mpv_cfg = MpvConfig {
        socket: paths.runtime_dir.join(format!("pova-{}.sock", std::process::id())),
        volume: state.volume.unwrap_or(cfg.volume).min(100),
        extra_args: cfg.mpv_args.clone(),
        pid_out: viz.mpv_pid.clone(),
    };
    let player_task = tokio::spawn(mpv::run(mpv_cfg, player_cmd_rx, player_ev_tx));

    let (ytm_tx, ytm_req_rx) = unbounded_channel();
    let (ytm_ev_tx, mut ytm_rx) = unbounded_channel();
    tokio::spawn(ytm::run(paths.cookie_file(), ytm_req_rx, ytm_ev_tx));

    let (lyrics_tx, lyrics_req_rx) = unbounded_channel();
    let (lyrics_ev_tx, mut lyrics_rx) = unbounded_channel();
    tokio::spawn(lyrics::run(lyrics_req_rx, lyrics_ev_tx));

    let (art_tx, art_req_rx) = unbounded_channel();
    let (art_ev_tx, mut art_rx) = unbounded_channel();
    tokio::spawn(art::run(paths.cache_dir.join("art"), art_req_rx, art_ev_tx));

    // ── terminal ──
    let (mut terminal, enhanced) = setup_terminal()?;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal(enhanced);
        default_hook(info);
    }));

    let picker = if cfg.album_art {
        Some(make_picker(&cfg.image_protocol))
    } else {
        None
    };
    // Terminals that answer the graphics query late (e.g. inside tmux) would
    // otherwise have the replies glued to the user's first key presses.
    drain_stdin(Duration::from_millis(150));

    // Image resize/encode runs off the UI thread.
    let (resize_tx, mut resize_req_rx) = unbounded_channel::<ResizeRequest>();
    let (resized_tx, mut resized_rx) = unbounded_channel();
    std::thread::Builder::new().name("art-resize".into()).spawn(move || {
        while let Some(req) = resize_req_rx.blocking_recv() {
            if resized_tx.send(req.resize_encode()).is_err() {
                break;
            }
        }
    })?;
    let art_proto = picker.as_ref().map(|_| ThreadProtocol::new(resize_tx, None));

    let channels = Channels {
        player: player_tx.clone(),
        ytm: ytm_tx,
        lyrics: lyrics_tx,
        art: art_tx,
    };
    let mut app = App::new(cfg, paths, state, keymap, channels, picker, art_proto);
    for w in key_warnings {
        app.flash(w, true);
    }
    if let Some(q) = cli.play {
        app.play_query(q);
    }

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / 60.0));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    // Closing the terminal window (SIGHUP) or `kill` (SIGTERM) must still
    // tear down mpv and restore the terminal.
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;

    let result: anyhow::Result<()> = async {
        loop {
            tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(ev)) => app.on_terminal(ev),
                    Some(Err(e)) => return Err(e).context("reading terminal events"),
                    None => break,
                },
                _ = sigterm.recv() => app.quit = true,
                _ = sighup.recv() => app.quit = true,
                Some(ev) = player_rx.recv() => app.on_player(ev),
                Some(ev) = ytm_rx.recv() => app.on_ytm(ev),
                Some(ev) = lyrics_rx.recv() => app.on_lyrics(ev),
                Some(ev) = art_rx.recv() => app.on_art(ev),
                Some(done) = resized_rx.recv() => {
                    match done {
                        Ok(resp) => {
                            if let Some(a) = app.art.as_mut() {
                                a.update_resized_protocol(resp);
                            }
                        }
                        Err(e) => tracing::warn!("art encode: {e}"),
                    }
                    app.dirty = true;
                }
                _ = tick.tick() => {
                    if viz.frames.has_changed().unwrap_or(false) {
                        app.viz_frame = viz.frames.borrow_and_update().clone();
                        if app.show_viz {
                            app.dirty = true;
                        }
                    }
                    // Redraw on change; spinners animate at ~8 fps.
                    let animate = app.animating() && last_draw.elapsed() > Duration::from_millis(120);
                    if app.tick() || animate {
                        let mut want = 0;
                        terminal.draw(|f| want = ui::draw(f, &mut app))?;
                        if want > 0 {
                            viz.set_bars(want);
                        }
                        last_draw = Instant::now();
                    }
                }
            }
            if app.quit {
                break;
            }
        }
        Ok(())
    }
    .await;

    // ── teardown ──
    app.save_state();
    let _ = player_tx.send(PlayerCmd::Quit);
    let _ = tokio::time::timeout(Duration::from_secs(2), player_task).await;
    viz.shutdown();
    restore_terminal(enhanced);
    tracing::info!("bye");
    result
}
