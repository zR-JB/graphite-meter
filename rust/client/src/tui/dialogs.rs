//! The server chooser, and help listing a screen's keys.
use super::{
    App, Screen,
    frame::highlight,
    keys::{Action, Binding, Key},
    theme::Palette,
};
use crate::{
    report::vocabulary as words,
    text::{Line, Style},
};
use graphite_meter_proto::catalog::{MAX_SELECTED, ServerId};

/// The chooser's focused catalogue entry and the one to four servers it would select.
pub struct Chooser {
    row: usize,
    draft: Vec<ServerId>,
}

impl App {
    /// Opens the chooser over the catalogue, or once the check in progress brings it.
    pub(super) fn open_chooser(&mut self) {
        let catalogue = self.view.catalogue.clone();
        let (later, notice) = match catalogue.len() {
            _ if self.checking() => (true, "Test servers open when the path check finishes."),
            0 => {
                self.recheck_soon();
                (true, "Loading servers…")
            }
            1 => (false, "This catalogue offers one server."),
            _ => {
                let selected = |id: &ServerId| match self.config.servers.is_empty() {
                    true => self.view.servers.iter().any(|server| server.id == *id),
                    false => self.config.servers.contains(id),
                };
                let draft = catalogue
                    .iter()
                    .map(|entry| entry.id.clone())
                    .filter(selected)
                    .collect();
                self.screen = Screen::Chooser(Chooser { row: 0, draft });
                (false, "Choose up to 4. Their speeds are combined.")
            }
        };
        (self.setup.chooser, self.notice) = (later, notice.into());
    }

    pub(super) fn chooser_key(&mut self, action: Action, key: Key) {
        let Screen::Chooser(chooser) = &mut self.screen else { return };
        let catalogue = &self.view.catalogue;
        match action {
            Action::Move => {
                let last = catalogue.len().saturating_sub(1);
                chooser.row = chooser.row.saturating_add_signed(key.step()).min(last);
            }
            Action::Select => {
                let Some(entry) = catalogue.get(chooser.row) else { return };
                match chooser.draft.iter().position(|id| *id == entry.id) {
                    Some(_) if chooser.draft.len() == 1 => self.notice = "At least one server takes the test.".into(),
                    Some(at) => {
                        chooser.draft.remove(at);
                    }
                    None if chooser.draft.len() < MAX_SELECTED => chooser.draft.push(entry.id.clone()),
                    None => self.notice = "At most four servers share one test.".into(),
                }
            }
            Action::Apply => {
                self.config.servers = std::mem::take(&mut chooser.draft);
                (self.screen, self.notice) = (Screen::Setup, "Checking the selected servers…".into());
                self.recheck_soon();
            }
            Action::Cancel => (self.screen, self.notice) = (Screen::Setup, "Server selection unchanged.".into()),
            _ => {}
        }
    }

    /// The catalogue around the focused entry in a panel, each entry with its address beneath.
    pub(super) fn chooser_body(&self, chooser: &Chooser, width: usize, height: usize) -> Vec<Line> {
        let (catalogue, palette) = (&self.view.catalogue, &self.palette);
        let shown = (height.saturating_sub(4) / 2).max(2);
        let first = chooser
            .row
            .saturating_sub(shown / 2)
            .min(catalogue.len().saturating_sub(shown));
        let mut lines = Vec::new();
        for (index, entry) in catalogue.iter().enumerate().skip(first).take(shown) {
            let mut label = words::server(&entry.name, &entry.location);
            if let Some(server) = self.view.servers.iter().find(|server| server.id == entry.id) {
                label = format!("{label} · {}", self.readiness(server).label());
            }
            let line = self
                .checkbox(chooser.draft.contains(&entry.id))
                .and(format!(" {label}"), Style::default());
            lines.push(match index == chooser.row {
                true => Line::plain("› ").with(highlight(line, palette.selected)),
                false => Line::plain("  ").with(line),
            });
            lines.push(Line::plain("    ").and(entry.url.resolve(&self.config.url).to_string(), palette.muted));
        }
        let title = format!("Test servers · {} selected", chooser.draft.len());
        self.panel(&title, lines, width, 0)
    }
}

/// Help on one line: each key and what it does.
pub fn line(bindings: &[Binding], palette: &Palette) -> Line {
    let mut line = Line::default();
    for (key, does) in bindings.iter().filter_map(|binding| binding.help) {
        if !line.0.is_empty() {
            line = line.and(" • ", palette.border);
        }
        line = line
            .and(key, palette.text)
            .and(" ", Style::default())
            .and(does, palette.muted);
    }
    line
}

/// Help with every key: columns of three, each key beside what it does.
pub fn columns(bindings: &[Binding], palette: &Palette) -> Vec<Line> {
    let helps: Vec<_> = bindings.iter().filter_map(|binding| binding.help).collect();
    let mut lines = vec![Line::default(); helps.len().min(3)];
    for (index, column) in helps.chunks(3).enumerate() {
        let widest = |part: fn(&(&'static str, &'static str)) -> &'static str| {
            column.iter().map(|help| crate::text::width(part(help))).max()
        };
        let (keys, does) = (widest(|help| help.0).unwrap_or(0), widest(|help| help.1).unwrap_or(0));
        for (row, line) in lines.iter_mut().enumerate() {
            let (key, what) = column.get(row).copied().unwrap_or_default();
            let gap = if index > 0 { "    " } else { "" };
            let cell = Line::styled(gap, palette.border).with(Line::styled(key, palette.text).pad(keys));
            *line = std::mem::take(line)
                .with(cell)
                .and(" ", Style::default())
                .with(Line::styled(what, palette.muted).pad(does));
        }
    }
    lines.into_iter().map(Line::trimmed).collect()
}
