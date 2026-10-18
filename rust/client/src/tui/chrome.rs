//! Drives the terminal beyond the frame, its title (OSC 2), progress bar (OSC 9;4) and links over drawn text (OSC 8).
use crate::text::{self, Line, Profile};
use std::io::Write;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Chrome {
    pub title: String,
    pub progress: Progress,
    pub links: Vec<Link>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Progress {
    #[default]
    None,
    /// The bar is under way without a known share.
    Busy,
    /// The bar shows this percentage done.
    Share(u8),
}

/// Links `text`, drawn from `column` of `row`, both from zero, to `url`.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub row: u16,
    pub column: u16,
    pub url: String,
    pub text: Line,
}

impl Progress {
    /// Clears the bar.
    pub const CLEAR: &[u8] = b"\x1b]9;4;0\x07";
}

impl Chrome {
    /// Emits the sequences that change what `shown` set, or set everything but an absent bar. Links are rewritten
    /// once `redrawn` cells may cover them.
    pub fn bytes(&self, shown: Option<&Self>, redrawn: bool, profile: Profile) -> Vec<u8> {
        let mut out = Vec::new();
        if shown.is_none_or(|shown| shown.title != self.title) {
            let title: String = self.title.chars().filter(|c| !c.is_control()).collect();
            let _ = write!(out, "\x1b]2;{title}\x07");
        }
        if shown.map_or(Progress::None, |shown| shown.progress) != self.progress {
            let _ = match self.progress {
                Progress::None => out.write_all(Progress::CLEAR),
                Progress::Busy => write!(out, "\x1b]9;4;3\x07"),
                Progress::Share(share) => write!(out, "\x1b]9;4;1;{share}\x07"),
            };
        }
        let relink = redrawn || shown.is_none_or(|shown| shown.links != self.links);
        for link in self.links.iter().filter(|_| relink) {
            let url: String = link.url.chars().filter(char::is_ascii_graphic).collect();
            let _ = write!(
                out,
                "\x1b[{};{}H\x1b]8;;{url}\x1b\\",
                link.row + 1,
                link.column + 1
            );
            let _ = text::write(std::slice::from_ref(&link.text), profile, &mut out);
            out.pop();
            out.extend_from_slice(b"\x1b]8;;\x1b\\");
        }
        out
    }
}

/// Tells whether the terminal draws the progress bar of OSC 9;4. Elsewhere it may print the sequence as text.
pub fn progress(var: impl Fn(&str) -> Option<String>) -> bool {
    let program = var("TERM_PROGRAM").unwrap_or_default();
    let version = || {
        let version = var("TERM_PROGRAM_VERSION").unwrap_or_default();
        let parts = version
            .split('.')
            .map(|part| part.parse::<u32>().unwrap_or(0));
        parts
            .chain(std::iter::repeat(0))
            .take(3)
            .collect::<Vec<_>>()
    };
    var("WT_SESSION").is_some_and(|session| !session.is_empty())
        || var("ConEmuANSI").is_some_and(|ansi| ansi == "ON")
        || program == "ghostty"
        || var("TERM").is_some_and(|term| term.contains("ghostty"))
        || program == "iTerm.app" && version() >= vec![3, 6, 6]
}
