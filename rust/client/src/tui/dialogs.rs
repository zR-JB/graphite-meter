//! The server chooser, and the sign-in prompt's keys and the browser it opens.
use super::{App, Effect, Overlay, Target, frame::Entry, keys::Action, keys::Key};
use crate::{
    controller::Command,
    report::vocabulary as words,
    text::{Line, Style},
};
use graphite_meter_proto::catalog::{MAX_SELECTED, ServerId};
use std::{process::Stdio, time::Instant};

/// The chooser's focused catalogue entry and the up to four servers it would select; none takes the default ones.
pub struct Chooser {
    row: usize,
    draft: Vec<ServerId>,
}

impl App {
    /// Opens the chooser over the catalogue, or once the check in progress brings it.
    pub(super) fn open_chooser(&mut self) {
        let catalogue = self.view.catalogue.clone();
        let later = match catalogue.len() {
            _ if self.checking() => true,
            0 => {
                self.recheck_soon();
                true
            }
            1 => {
                self.notify(Line::styled("This server offers one test server.", self.palette.muted));
                false
            }
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
                (self.overlay, self.scroll) = (Overlay::Chooser(Chooser { row: 0, draft }), 0);
                false
            }
        };
        if later {
            self.notify(Line::styled("Test servers open once the check finishes.", self.palette.muted));
        }
        self.setup.chooser = later;
    }

    pub(super) fn chooser_key(&mut self, action: Action, key: Key) {
        let Overlay::Chooser(chooser) = &mut self.overlay else { return };
        let catalogue = &self.view.catalogue;
        match action {
            Action::Move => {
                let last = catalogue.len().saturating_sub(1);
                chooser.row = chooser.row.saturating_add_signed(key.step()).min(last);
            }
            Action::Select => self.select_entry(),
            Action::Apply => {
                self.config.servers = std::mem::take(&mut chooser.draft);
                self.overlay = Overlay::Settings;
                self.recheck_soon();
            }
            Action::Cancel => self.overlay = Overlay::Settings,
            _ => {}
        }
    }

    /// A click on entry `index`: focuses it and selects or deselects it.
    pub(super) fn click_entry(&mut self, index: usize) {
        if let Overlay::Chooser(chooser) = &mut self.overlay {
            chooser.row = index;
        }
        self.select_entry();
    }

    /// Selects the focused entry, or deselects it; at most four share a test.
    fn select_entry(&mut self) {
        let Overlay::Chooser(chooser) = &mut self.overlay else { return };
        let Some(entry) = self.view.catalogue.get(chooser.row) else { return };
        match chooser.draft.iter().position(|id| *id == entry.id) {
            Some(at) => _ = chooser.draft.remove(at),
            None if chooser.draft.len() < MAX_SELECTED => chooser.draft.push(entry.id.clone()),
            None => self.notify(Line::styled("At most four servers share one test.", self.palette.warn)),
        }
    }

    pub(super) fn chooser_title(&self) -> String {
        let Overlay::Chooser(chooser) = &self.overlay else { return String::new() };
        format!("Test servers · {} selected", chooser.draft.len())
    }

    /// The catalogue as the chooser's entries, each with its address and readiness, and the focused one's index.
    pub(super) fn chooser_entries(&self) -> (Vec<Entry>, usize) {
        let Overlay::Chooser(chooser) = &self.overlay else { return (Vec::new(), 0) };
        let palette = &self.palette;
        let mut entries = vec![Entry::Text(Line::styled("Choose up to four. Their speeds add up.", palette.muted))];
        let mut focus = 0;
        for (index, entry) in self.view.catalogue.iter().enumerate() {
            if index == chooser.row {
                focus = entries.len();
            }
            let checked = self.checkbox(chooser.draft.contains(&entry.id));
            let label = checked
                .and(" ", Style::default())
                .and(words::server(&entry.name, &entry.location), palette.text);
            let server = self.view.servers.iter().find(|server| server.id == entry.id);
            let value = match server {
                Some(server) => Line::styled(self.readiness(server).label(), palette.muted),
                None => Line::default(),
            };
            entries.push(Entry::Item { label, value, target: Some(Target::Entry(index)) });
        }
        (entries, focus)
    }
}

/// The sign-in prompt's arrival and whether its page was opened.
pub struct SignIn {
    pub(super) since: Instant,
    pub(super) opened: bool,
}

impl App {
    pub(super) fn sign_in_key(&mut self, action: Action) -> Vec<Effect> {
        let (Overlay::SignIn(sign_in), Some(prompt)) = (&mut self.overlay, &self.view.sign_in) else {
            return Vec::new();
        };
        match action {
            Action::Browse => {
                sign_in.opened = true;
                vec![Effect::OpenBrowser(prompt.url.clone())]
            }
            Action::Cancel => {
                self.overlay = Overlay::None;
                self.notify(Line::styled("Sign-in canceled. v requests a new code.", self.palette.muted));
                Effect::command(Command::Stop)
            }
            _ => Vec::new(),
        }
    }
}

/// Opens the HTTPS page `url` with the platform's opener, which is reaped in the background.
pub(super) fn browse(url: &str) {
    if !url.starts_with("https://") {
        return;
    }
    let (opener, args): (&str, &[&str]) = match () {
        _ if cfg!(windows) => ("rundll32", &["url.dll,FileProtocolHandler"]),
        _ if cfg!(target_os = "macos") => ("open", &[]),
        _ => ("xdg-open", &[]),
    };
    let mut command = std::process::Command::new(opener);
    command.args(args).arg(url);
    command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    if let Ok(mut child) = command.spawn() {
        std::thread::spawn(move || child.wait());
    }
}
