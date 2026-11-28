//! Right-to-left text (Arabic, Persian, Hebrew) for terminals.
//!
//! Some terminals lay out right-to-left text themselves (kitty, Konsole,
//! GNOME Terminal and the other VTE ones): there pova must leave the text
//! alone, or it gets reversed twice. Others (Alacritty, foot, xterm) draw
//! characters in stored order, left to right, without joining Arabic
//! letters, so a lyric line comes out backwards. For those, [`lines`]
//! wraps a line to the width first (in reading order, so a long line breaks
//! where a reader expects), then joins the letters (their presentation
//! forms) and puts each piece in display order. Text without right-to-left
//! letters is returned untouched.

use std::sync::{LazyLock, OnceLock};

use crate::config::RtlText;

use ar_reshaper::ArabicReshaper;
use unicode_bidi::BidiInfo;

fn width(s: &str) -> usize {
    ratatui::text::Span::raw(s).width()
}

static REORDER: OnceLock<bool> = OnceLock::new();

/// Decide once, at start-up, whether pova reorders right-to-left text.
pub fn set_mode(setting: RtlText) {
    let _ = REORDER.set(match setting {
        RtlText::Pova => true,
        RtlText::Terminal => false,
        RtlText::Auto => !terminal_lays_out_rtl(|k| std::env::var(k).ok()),
    });
}

/// kitty, Konsole and VTE terminals order right-to-left text themselves.
/// The variables are inherited into tmux and screen, and the outer
/// terminal still does the layout there.
fn terminal_lays_out_rtl(var: impl Fn(&str) -> Option<String>) -> bool {
    var("KITTY_WINDOW_ID").is_some()
        || var("TERM").is_some_and(|t| t == "xterm-kitty")
        || var("KONSOLE_VERSION").is_some()
        || var("VTE_VERSION").is_some()
}

fn reorder() -> bool {
    *REORDER.get().unwrap_or(&false)
}

static RESHAPER: LazyLock<ArabicReshaper> = LazyLock::new(ArabicReshaper::default);

/// Hebrew, Arabic, Syriac, Thaana, NKo and the rest of the RTL blocks,
/// plus the Arabic presentation forms.
fn is_rtl(c: char) -> bool {
    matches!(c as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFF | 0x10800..=0x10FFF | 0x1E800..=0x1EFFF)
}

pub fn has_rtl(s: &str) -> bool {
    s.chars().any(is_rtl)
}

/// One line of text as it should be drawn, in pieces no wider than `max`
/// columns (one piece when it fits, or when `max` is 0).
pub fn lines(s: &str, max: usize) -> Vec<String> {
    if !reorder() {
        return vec![s.to_string()];
    }
    to_lines(s, max)
}

/// One line as it should be drawn, without wrapping.
pub fn visual(s: &str) -> String {
    if !reorder() {
        return s.to_string();
    }
    to_visual(s)
}

fn to_lines(s: &str, max: usize) -> Vec<String> {
    if !has_rtl(s) {
        return vec![s.to_string()];
    }
    wrap(s, max).iter().map(|piece| to_visual(piece)).collect()
}

/// Join the letters and reorder for display (one line, no wrapping).
fn to_visual(s: &str) -> String {
    if !has_rtl(s) {
        return s.to_string();
    }
    let shaped = RESHAPER.reshape(s);
    let info = BidiInfo::new(&shaped, None);
    let mut out = String::with_capacity(shaped.len());
    for para in &info.paragraphs {
        let (levels, runs) = info.visual_runs(para, para.range.clone());
        for run in runs {
            let text = &shaped[run.clone()];
            if levels[run.start].is_rtl() {
                // Reversed, so brackets flip to face the right way.
                out.extend(text.chars().rev().map(mirror));
            } else {
                out.push_str(text);
            }
        }
    }
    out
}

/// The mirrored bracket, as a right-to-left run needs it.
fn mirror(c: char) -> char {
    match c {
        '(' => ')',
        ')' => '(',
        '[' => ']',
        ']' => '[',
        '{' => '}',
        '}' => '{',
        '<' => '>',
        '>' => '<',
        '«' => '»',
        '»' => '«',
        '‹' => '›',
        '›' => '‹',
        c => c,
    }
}

/// Greedy word wrap in reading order. Words wider than `max` stay whole.
fn wrap(s: &str, max: usize) -> Vec<String> {
    if max == 0 || width(s) <= max {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if !cur.is_empty() && width(&cur) + 1 + width(word) > max {
            out.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() || out.is_empty() {
        out.push(cur);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kitty_konsole_and_vte_lay_out_rtl_themselves() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
        };
        assert!(terminal_lays_out_rtl(env(&[
            ("KITTY_WINDOW_ID", "1"),
            ("TERM", "tmux-256color")
        ])));
        assert!(terminal_lays_out_rtl(env(&[("TERM", "xterm-kitty")])));
        assert!(terminal_lays_out_rtl(env(&[("KONSOLE_VERSION", "250402")])));
        assert!(terminal_lays_out_rtl(env(&[("VTE_VERSION", "7800")])));
        assert!(!terminal_lays_out_rtl(env(&[("TERM", "alacritty")])));
        assert!(!terminal_lays_out_rtl(env(&[("TERM", "foot")])));
    }

    #[test]
    fn latin_text_is_untouched() {
        assert_eq!(to_lines("Hold me now", 4), ["Hold me now"]);
        assert_eq!(to_visual("ça va?"), "ça va?");
    }

    #[test]
    fn arabic_reads_right_to_left_with_joined_letters() {
        // "سلام" (salaam): joined forms, last letter drawn first.
        let v = to_visual("سلام");
        assert_eq!(v.chars().count(), 3); // lam-alef becomes one ligature
        assert!(v.chars().all(|c| ('\u{FB50}'..='\u{FEFF}').contains(&c)));
        assert_eq!(v.chars().last(), Some('\u{FEB3}')); // initial seen, on the right
        // Words come in right-to-left order: the first word ends up last.
        let two = to_visual("يا حبيبي");
        assert!(two.ends_with(&to_visual("يا")));
    }

    #[test]
    fn mixed_lines_keep_latin_words_readable() {
        let v = to_visual("أحبك baby");
        assert!(v.contains("baby"));
        assert!(v.starts_with("baby"));
    }

    #[test]
    fn brackets_face_the_right_way() {
        // Reading right to left, "(معاك)" opens on the right: drawn "(…)".
        let v = to_visual("تملي (معاك)");
        assert!(v.starts_with('(') && v.contains(')'));
        assert!(!v.contains(")ﻙ"));
    }

    #[test]
    fn long_lines_wrap_in_reading_order() {
        let s = "واحد اثنان ثلاثة أربعة";
        let w = to_lines(s, 12);
        assert!(w.len() >= 2);
        // The first piece shown holds the first words read.
        assert_eq!(w[0], to_visual(&wrap(s, 12)[0]));
        assert!(wrap(s, 12)[0].starts_with("واحد"));
        assert!(w.iter().all(|p| width(p) <= 12));
    }
}
