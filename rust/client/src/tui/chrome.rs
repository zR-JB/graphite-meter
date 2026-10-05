//! What the terminal shows beyond the frame, as the sequences that change it: the window title (OSC 2), the progress
//! bar (OSC 9;4) and links over drawn text (OSC 8).
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
    /// Under way without a known share.
    Busy,
    /// The percentage done.
    Share(u8),
}

/// `text` drawn from `column` of `row`, both from zero, linking to `url`.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    pub row: u16,
    pub column: u16,
    pub url: String,
    pub text: Line,
}

impl Chrome {
    /// The sequences that change what `shown` set, or set everything; links are written each time, since a draw may
    /// cover them.
    pub fn bytes(&self, shown: Option<&Self>, profile: Profile) -> Vec<u8> {
        let mut out = Vec::new();
        if shown.is_none_or(|shown| shown.title != self.title) {
            let title: String = self.title.chars().filter(|c| !c.is_control()).collect();
            let _ = write!(out, "\x1b]2;{title}\x07");
        }
        if shown.is_none_or(|shown| shown.progress != self.progress) {
            let _ = match self.progress {
                Progress::None => write!(out, "\x1b]9;4;0\x07"),
                Progress::Busy => write!(out, "\x1b]9;4;3\x07"),
                Progress::Share(share) => write!(out, "\x1b]9;4;1;{share}\x07"),
            };
        }
        for link in &self.links {
            let url: String = link.url.chars().filter(char::is_ascii_graphic).collect();
            let _ = write!(out, "\x1b[{};{}H\x1b]8;;{url}\x1b\\", link.row + 1, link.column + 1);
            let _ = text::write(std::slice::from_ref(&link.text), profile, &mut out);
            out.pop();
            out.extend_from_slice(b"\x1b]8;;\x1b\\");
        }
        out
    }
}
