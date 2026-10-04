//! Terminal input owns no measurement IO and never blocks its producer. The view follows Go's
//! TUI (model.go and run.go): the controller's snapshots stand for its events.
mod keys;
mod run;
mod setup;
mod view;
use crate::{
    Error,
    config::Config,
    model::{Phase, ServerSummary, Snapshot, Stage},
    report::{ansi, line, plain, run_servers},
    theme::Theme,
    vocabulary::BLOCKED,
};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event, EventStream,
        KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use keys::*;
use run::Run;
use setup::{Edit, Setting};
use std::{
    io::{self, IsTerminal, Write},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    time::Instant,
};

/// How the interface ended, with the finished run Go's final report prints.
#[derive(Clone, Debug, Default)]
pub struct Exit {
    pub interrupted: bool,
    pub running: bool,
    pub shown: Option<Snapshot>,
}

#[derive(Clone, Debug)]
pub enum Command {
    Run(Config),
    Verify(Config),
    Cancel,
    OpenBrowser,
}

const MAX_TEXT: usize = 4096;
/// Go's prepareDebounce: paths are checked again once path settings stop changing.
const RECHECK_DELAY: Duration = Duration::from_millis(350);
/// Go's PreparationFreshness: checked paths serve a run this long.
const FRESHNESS: Duration = Duration::from_secs(30);
/// The controller's words for an approval that expired, which Go reads as a sign-in to repeat.
const SIGN_IN_EXPIRED: &str = "Sign-in expired. Press v to request a new code.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Popup {
    #[default]
    None,
    Servers,
    Details,
}

/// Go's prepareState, and the readiness of a checked server in the order of its label.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathState {
    Ready,
    Checking,
    Stale,
    Failed,
    SignIn,
}

impl PathState {
    /// Go's pathLabels.
    fn label(self) -> &'static str {
        ["Ready", "Checking", "Recheck needed", "Failed", "Sign in"][self as usize]
    }
}

/// Go's terminal progress bar (OSC 9;4) when it shows none.
const NO_PROGRESS: &str = "\x1b]9;4;0\x07";

/// Go's window title, progress bar and sign-in link; dropping it clears them and restores the terminal.
struct Chrome {
    title: String,
    progress: String,
}

impl Chrome {
    fn show(&mut self, ui: &Ui, drawn: &ratatui_core::buffer::Buffer) -> io::Result<()> {
        let mut sequences = self.update(format!("Graphite Meter · {}", ui.status_label()), ui.progress());
        if let Some(auth) = &ui.snapshot.auth {
            let url: String = auth.browser_url.chars().filter(|c| c.is_ascii_graphic()).collect();
            // The drawn rows that hold Go's signInLink, hard-wrapped at the frame's inner width.
            let inner = usize::from(ui.size.0).max(view::MIN_WIDTH) - 2;
            let link: Vec<_> = ui.sign_in_link(inner).iter().map(plain).collect();
            for (row, cells) in drawn.content.chunks(usize::from(drawn.area.width).max(1)).enumerate() {
                let text: String = cells.iter().map(|cell| cell.symbol()).collect();
                if link.iter().any(|chunk| chunk == text.trim()) {
                    let text = ansi(&[line(text.trim(), ui.theme.accent)]);
                    sequences.push_str(&format!("\x1b[{};2H\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\", row + 1));
                }
            }
        }
        let mut stdout = io::stdout().lock();
        stdout.write_all(sequences.as_bytes())?;
        stdout.flush()
    }

    fn update(&mut self, title: String, progress: String) -> String {
        let mut sequences = String::new();
        if title != self.title {
            sequences.push_str(&format!("\x1b]2;{title}\x07"));
            self.title = title;
        }
        if progress != self.progress {
            sequences.push_str(&progress);
            self.progress = progress;
        }
        sequences
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        let sequences = self.update(String::new(), NO_PROGRESS.into());
        let _ = io::stdout().lock().write_all(sequences.as_bytes());
        restore();
    }
}

fn restore() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture, LeaveAlternateScreen);
}

pub async fn run(
    config: Config,
    mut snapshots: watch::Receiver<Snapshot>,
    commands: mpsc::Sender<Command>,
    mut interrupts: mpsc::Receiver<()>,
) -> Result<Exit, Error> {
    // Like Bubble Tea, crossterm reads keys from the terminal when stdin is redirected.
    if !io::stdout().is_terminal() {
        return Err("interactive mode requires a terminal on stdout".into());
    }
    let mut chrome = Chrome { title: String::new(), progress: NO_PROGRESS.into() };
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        hook(info);
    }));
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = ratatui_core::terminal::Terminal::new(ratatui_crossterm::CrosstermBackend::new(io::stdout()))?;
    // As Go's, the first frame does not wait for the background query: its answer arrives among the
    // keys whenever it comes. The clear wipes whatever a terminal that ignores the query printed.
    #[cfg(unix)]
    io::stdout().write_all(crate::theme::QUERY)?;
    execute!(io::stdout(), EnableBracketedPaste, EnableMouseCapture, Clear(ClearType::All))?;
    let mut ui = Ui::new(config, snapshots.borrow_and_update().clone());
    let mut events = EventStream::new();
    let mut refresh = tokio::time::interval(Duration::from_millis(33));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (mut dirty, mut changed, mut stale) = (true, false, false);
    loop {
        tokio::select! {
            result = snapshots.changed() => {
                result.map_err(|_| "measurement controller stopped")?;
                changed = true;
            }
            Some(()) = interrupts.recv() => {
                if ui.interrupt(&commands) {
                    return Ok(ui.exit());
                }
                dirty = true;
            }
            _ = refresh.tick() => {
                if changed {
                    ui.update(snapshots.borrow_and_update().clone());
                    (changed, dirty) = (false, true);
                    if ui.quitting && !ui.running() {
                        return Ok(ui.exit());
                    }
                }
                dirty |= ui.recheck(&commands);
                // As Go schedules a frame for it, checked paths read as needing a recheck once they expire.
                dirty |= std::mem::replace(&mut stale, ui.stale()) != stale;
                if ui.animating() {
                    ui.frame();
                    dirty = true;
                }
                if dirty {
                    let drawn = terminal.draw(|frame| ui.draw(frame))?;
                    chrome.show(&ui, drawn.buffer)?;
                    dirty = false;
                }
            }
            event = events.next() => {
                let Some(event) = event else { return Err("terminal input closed".into()); };
                match event? {
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        if ui.key(key, &commands) {
                            return Ok(ui.exit());
                        }
                        dirty = true;
                    }
                    Event::Paste(text) => {
                        ui.paste(&text);
                        dirty = true;
                    }
                    Event::Mouse(mouse) => dirty |= ui.mouse(mouse.kind),
                    Event::Resize(_, _) => dirty = true,
                    _ => {}
                }
            }
        }
    }
}

#[derive(Default)]
struct Ui {
    config: Config,
    /// The settings of the last run or check the controller was asked for.
    requested: Config,
    snapshot: Snapshot,
    /// Go's runState: what the view keeps of the run it shows.
    run: Run,
    /// The finished run a run again replaces, shown until the new one starts, as Go keeps m.run.
    previous: Option<(Snapshot, Run)>,
    /// Go's preparedRun: the catalogue as the last settled check left it.
    prepared: Vec<ServerSummary>,
    checked_at: Option<Instant>,
    checked_key: Option<Config>,
    check_started: Option<Instant>,
    /// Why the last settled check failed: Go's prepareFailed and its error.
    check_error: Option<String>,
    theme: Theme,
    size: (u16, u16),
    spin: usize,
    advanced: bool,
    row: usize,
    edit: Option<Edit>,
    popup: Popup,
    draft: Vec<String>,
    server_row: usize,
    open_chooser: bool,
    /// Go's body viewport offset.
    body: usize,
    help: bool,
    notice: String,
    reset_prompt: bool,
    recheck: Option<Instant>,
    awaiting: bool,
    /// A run was asked for, or is shown: Go's m.next or m.run.
    live: bool,
    /// A requested run that has not started: the check it replaces is not a return to setup.
    starting: bool,
    stop_prompt: bool,
    quitting: bool,
    interrupted: bool,
    latency_pick: Option<String>,
    /// Go's auth.opened: the sign-in page was opened for the code shown.
    opened: bool,
    /// The sign-in was canceled or expired: Go's prepareSignIn without a code.
    signed_out: bool,
    /// The answer to the background query, while it arrives as keys.
    answer: Option<String>,
}

/// Distinct prepared path labels, in catalogue order, shared by setup and run details.
fn path_summary<'a>(servers: impl Iterator<Item = &'a ServerSummary>, latency: bool) -> String {
    let mut paths = Vec::new();
    for server in servers {
        let path = if latency {
            server.latency.as_ref().map(crate::vocabulary::latency_path)
        } else {
            server.throughput.as_ref().map(crate::vocabulary::throughput_path)
        };
        if let Some(path) = path.filter(|path| !paths.contains(path)) {
            paths.push(path);
        }
    }
    paths.join(" / ")
}

impl Ui {
    /// Setup whose first snapshot settles as any later one does, however early its check ended.
    fn new(config: Config, snapshot: Snapshot) -> Self {
        let mut ui = Self {
            requested: config.clone(),
            run: Run::new(config.clone()),
            config,
            theme: Theme::terminal(),
            size: (80, 24),
            check_started: Some(Instant::now()),
            ..Self::default()
        };
        ui.update(snapshot);
        ui
    }

    /// Go's handlePreparation, handleEvents and the sign-in replies, from a new snapshot.
    fn update(&mut self, mut snapshot: Snapshot) {
        match (&self.snapshot.auth, &snapshot.auth) {
            (before, Some(auth)) if before.as_ref().is_none_or(|before| before.code != auth.code) => {
                (self.opened, self.body) = (false, 0);
                self.notice = "Check the code, then press enter to open the sign-in page.".into();
            }
            (Some(_), None) if snapshot.phase.busy() && !self.signed_out => {
                self.notice = "Signed in. Checking the authenticated paths…".into();
            }
            _ => {}
        }
        // In Go's event order: the run starts, its servers fail, then it finishes, which clears the notice.
        if self.live && !self.quitting && !self.snapshot.started() && snapshot.started() {
            (self.notice, self.body, self.previous) = ("Test started. Press esc to stop.".into(), 0, None);
        }
        let failed = snapshot.failures.iter().skip(self.snapshot.failures.len()).last();
        if let Some(failure) = failed.filter(|_| !self.quitting) {
            let name = crate::report::server_name(&snapshot, &failure.server_id);
            self.notice = format!("{name}: {}", failure.reason.label());
        }
        if self.live && self.snapshot.phase.live() && !snapshot.phase.live() && !self.quitting {
            self.notice.clear();
        }
        self.awaiting = false;
        self.stop_prompt &= snapshot.auth.is_none() && snapshot.phase.live();
        let was_live = self.live;
        self.live &= snapshot.phase != Phase::Checking || self.starting;
        self.starting &= snapshot.phase == Phase::Checking;
        self.latency_pick = self.latency_pick.take().filter(|id| snapshot.participants.contains(id));
        // As Go's startFailed, a run that never starts leaves the last results, or setup, in place.
        let unstarted = matches!(snapshot.phase, Phase::Failed | Phase::Cancelled) && !snapshot.started();
        if self.live && !self.quitting && unstarted {
            self.notice = match snapshot.phase {
                Phase::Cancelled => "Test stopped before it started.".into(),
                _ => snapshot.error.take().unwrap_or_default(),
            };
            // Go checks again after a failed start, which the results it keeps hide until esc checks anew, so
            // only setup checks here; a stopped start leaves a check it replaced spinning.
            if self.previous.is_none() && (snapshot.phase != Phase::Cancelled || self.check_started.is_some()) {
                self.recheck_soon();
            }
            match self.previous.take() {
                Some((previous, run)) => (snapshot, self.run) = (previous, run),
                None => self.live = false,
            }
        }
        if self.live != was_live {
            self.body = 0;
        }
        if snapshot.auth.is_some() || self.popup == Popup::Details && !self.live {
            self.popup = Popup::None;
        }
        if self.live && (snapshot.phase.live() || snapshot.started()) && self.previous.is_none() {
            self.run.observe(&snapshot);
        }
        self.snapshot = snapshot;
        self.settle();
        if self.open_chooser && !self.live && !self.checking() && !self.prepared.is_empty() {
            self.open_chooser = false;
            self.open_servers();
        }
    }

    /// Go's handlePreparation: a settled check's servers, and whether it failed or needs a sign-in.
    fn settle(&mut self) {
        if !self.live && self.snapshot.phase == Phase::Checking {
            self.check_started.get_or_insert_with(Instant::now);
        }
        let settled = matches!(self.snapshot.phase, Phase::Setup | Phase::Failed);
        if self.live || self.awaiting || self.recheck.is_some() || !settled {
            return;
        }
        self.prepared.clone_from(&self.snapshot.servers);
        self.checked_at = Some(self.check_started.take().unwrap_or_else(Instant::now));
        self.checked_key = Some(self.requested.preparation_key());
        let failed = self.snapshot.phase == Phase::Failed;
        let error = self.snapshot.error.clone().filter(|_| failed);
        if error.as_deref() == Some(SIGN_IN_EXPIRED) {
            self.signed_out = true;
            self.notice = SIGN_IN_EXPIRED.into();
        }
        self.check_error = error.filter(|error| error != SIGN_IN_EXPIRED);
    }

    /// Go's prepare state.
    fn prepare(&self) -> PathState {
        match () {
            _ if self.signed_out || self.snapshot.auth.is_some() && !self.live => PathState::SignIn,
            _ if self.checking() => PathState::Checking,
            _ if self.check_error.is_some() => PathState::Failed,
            _ => PathState::Ready,
        }
    }

    /// Setup's paths are being checked, or will be once edits settle; as Go keeps its prepare state
    /// until a run starts, a start that replaced an unsettled check keeps it checking.
    fn checking(&self) -> bool {
        match self.live {
            true => self.shown().is_none() && self.check_started.is_some(),
            false => self.awaiting || self.recheck.is_some() || self.snapshot.phase == Phase::Checking,
        }
    }

    /// Go's preparedRun.Servers: the selected servers the last check reached.
    fn checked(&self) -> impl Iterator<Item = &ServerSummary> + Clone {
        self.prepared.iter().filter(|server| server.has_check_result())
    }

    /// Go's canChooseServers.
    fn can_choose_servers(&self) -> bool {
        self.prepared.len() > 1
    }

    /// Go's m.run: the run the view shows, which a run again keeps until the new one starts.
    fn shown(&self) -> Option<(&Snapshot, &Run)> {
        match self.snapshot.started() {
            _ if !self.live => None,
            true => Some((&self.snapshot, &self.run)),
            false => self.previous.as_ref().map(|(snapshot, run)| (snapshot, run)),
        }
    }

    /// Go's multipleRunServers.
    fn several(&self) -> bool {
        self.shown()
            .is_some_and(|(snapshot, _)| run_servers(snapshot).len() > 1)
    }

    /// Go's latencyServer: the viewer's pick, or the run's focus.
    fn latency_server(&self) -> Option<&str> {
        let focus = self.shown().and_then(|(snapshot, _)| snapshot.latency_focus.as_deref());
        self.latency_pick.as_deref().or(focus)
    }

    /// Go's running: a run was asked for or goes on.
    fn running(&self) -> bool {
        self.snapshot.auth.is_none() && self.live && (self.awaiting || self.starting || self.snapshot.phase.live())
    }

    /// Go's animating: the spinner and the clocks move.
    fn animating(&self) -> bool {
        self.running()
            || self.shown().is_none() && (self.prepare() == PathState::Checking || self.snapshot.auth.is_some())
    }

    fn mouse(&mut self, kind: MouseEventKind) -> bool {
        let key = match kind {
            _ if self.edit.is_some() || self.snapshot.auth.is_some() || self.stop_prompt => return false,
            MouseEventKind::ScrollUp => "up",
            MouseEventKind::ScrollDown => "down",
            _ => return false,
        };
        self.scroll(key);
        true
    }

    fn frame(&mut self) {
        self.spin = self.spin.wrapping_add(1);
        if self.live && self.previous.is_none() {
            self.run.ease(&self.snapshot);
        }
    }

    /// Go's View progress bar: indeterminate while paths are checked, then the share of stage time done.
    fn progress(&self) -> String {
        match (self.running(), self.snapshot.started() && self.snapshot.phase.live()) {
            (false, _) => NO_PROGRESS.into(),
            (true, false) => "\x1b]9;4;3\x07".into(),
            (true, true) => format!("\x1b]9;4;1;{}\x07", self.run.progress(&self.snapshot)),
        }
    }

    fn exit(&self) -> Exit {
        let running = self.running();
        let shown = self.shown().filter(|_| !running).map(|(snapshot, _)| snapshot.clone());
        Exit { interrupted: self.interrupted, running, shown }
    }

    /// Go's quit: a run stops first.
    fn quit(&mut self, commands: &mpsc::Sender<Command>) -> bool {
        if !self.running() {
            return true;
        }
        self.send(Command::Cancel, commands);
        (self.quitting, self.stop_prompt) = (true, false);
        self.notice = "Stopping the test before quitting… ctrl+c quits at once.".into();
        false
    }

    /// Go's interrupt: a second one, or one outside a run, quits at once.
    fn interrupt(&mut self, commands: &mpsc::Sender<Command>) -> bool {
        self.interrupted |= self.running();
        self.quitting || self.quit(commands)
    }

    fn send(&mut self, command: Command, commands: &mpsc::Sender<Command>) -> bool {
        let requested = match &command {
            Command::Run(config) | Command::Verify(config) => Some(config.clone()),
            _ => None,
        };
        let sent = commands.try_send(command);
        match &sent {
            Ok(()) => {
                if let Some(requested) = requested {
                    (self.requested, self.recheck) = (requested, None);
                }
                self.awaiting = true;
            }
            Err(mpsc::error::TrySendError::Full(_)) => self.notice = "Controller is busy; try again.".into(),
            Err(mpsc::error::TrySendError::Closed(_)) => self.notice = "Controller is unavailable.".into(),
        }
        sent.is_ok()
    }

    /// Go's reprepare: the paths are checked again once settings stop changing.
    fn recheck_soon(&mut self) {
        self.recheck = Some(Instant::now() + RECHECK_DELAY);
        self.check_started.get_or_insert_with(Instant::now);
        self.signed_out = false;
    }

    /// Sends a settled re-check.
    fn recheck(&mut self, commands: &mpsc::Sender<Command>) -> bool {
        if self.recheck.take_if(|at| *at <= Instant::now()).is_none() {
            return false;
        }
        if self.send(Command::Verify(self.config.clone()), commands) {
            self.check_started = Some(Instant::now());
        }
        true
    }

    fn paste(&mut self, text: &str) {
        if self.snapshot.auth.is_none()
            && let Some(edit) = &mut self.edit
        {
            edit.error.clear();
            edit.insert(text);
        }
    }

    /// The background query's answer reaches crossterm as Alt+] and its characters; they are gathered
    /// up to its BEL or ST, which sets the palette, as Bubble Tea's parser takes the answer whenever it comes.
    fn answer(&mut self, key: KeyEvent, name: &str) -> bool {
        let Some(answer) = &mut self.answer else {
            self.answer = (name == "alt+]").then(String::new);
            return self.answer.is_some();
        };
        let plain = matches!(key.code, KeyCode::Char(_)) && !key.modifiers.contains(KeyModifiers::CONTROL);
        if plain && name != "alt+\\" {
            answer.extend(key.code.as_char());
            return true;
        }
        if (plain || name == "ctrl+g") && crate::theme::answered(answer) {
            self.theme = Theme::terminal();
        }
        self.answer = None;
        plain || name == "ctrl+g"
    }

    /// Go's handleKey, and its recheckIfPathsChanged after any key; true quits.
    fn key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> bool {
        let (name, popup, before) = (keys::name(key), self.popup, self.config.preparation_key());
        match popup {
            _ if self.answer(key, &name) => {}
            _ if ABORT.matches(&name) => return self.interrupt(commands),
            _ if self.edit.is_some() => self.edit_key(&name, key),
            _ if QUIT.matches(&name) => return self.quit(commands),
            _ if HELP.matches(&name) && !self.stop_prompt => self.help = !self.help,
            Popup::Details if CLOSE.matches(&name) || DETAILS.matches(&name) => {
                (self.popup, self.body) = (Popup::None, 0)
            }
            Popup::Details if SCROLL.matches(&name) || PAGE.matches(&name) => self.scroll(&name),
            Popup::Servers => self.chooser_key(&name),
            Popup::Details => {}
            Popup::None if self.stop_prompt => {
                self.stop_prompt = false;
                if !CONFIRM_STOP.matches(&name) {
                    self.notice = "Test continues.".into();
                } else if self.send(Command::Cancel, commands) {
                    self.notice = "Stopping the test…".into();
                }
            }
            Popup::None if PAGE.matches(&name) || self.shown().is_some() && SCROLL.matches(&name) => self.scroll(&name),
            Popup::None if self.snapshot.auth.is_some() && self.shown().is_none() => self.sign_in_key(&name, commands),
            Popup::None if self.live => self.run_key(&name, commands),
            Popup::None if self.prepare() == PathState::SignIn && START.matches(&name) => {
                self.notice = format!("{BLOCKED}: sign in first. Press v to request a new code.");
            }
            Popup::None => self.setup_key(&name, commands),
        }
        if self.config.preparation_key() != before {
            self.recheck_soon();
        }
        false
    }

    /// Go's handleEditKey: Esc cancels, Enter applies, and other keys edit.
    fn edit_key(&mut self, name: &str, key: KeyEvent) {
        let Some(edit) = &mut self.edit else { return };
        if DISCARD.matches(name) {
            (self.edit, self.notice) = (None, "Edit canceled.".into());
        } else if APPLY.matches(name) {
            let (setting, text) = (edit.setting, edit.text());
            match self.commit_edit(setting, &text) {
                Ok(()) => self.edit = None,
                Err(error) => {
                    self.notice.clone_from(&error);
                    if let Some(edit) = &mut self.edit {
                        edit.error = error;
                    }
                }
            }
        } else {
            let plain = !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
            let typed = key.code.as_char().filter(|_| plain);
            edit.error.clear();
            edit.key(name, typed);
        }
    }

    /// Go's handleSignInKey.
    fn sign_in_key(&mut self, name: &str, commands: &mpsc::Sender<Command>) {
        if OPEN_SIGN_IN.matches(name) && self.send(Command::OpenBrowser, commands) {
            (self.opened, self.notice) = (true, "Sign-in page opened in the browser.".into());
        } else if CANCEL_SIGN_IN.matches(name) && self.send(Command::Cancel, commands) {
            (self.signed_out, self.live) = (true, false);
            self.notice = "Sign-in canceled. Press v to request a new code.".into();
        }
    }

    /// Go's handleRunKey.
    fn run_key(&mut self, name: &str, commands: &mpsc::Sender<Command>) {
        let finished = self.shown().is_some() && !self.running();
        if DETAILS.matches(name) {
            (self.popup, self.body) = (Popup::Details, 0);
        } else if self.several() && LATENCY_SERVER.matches(name) {
            let Some((snapshot, _)) = self.shown() else { return };
            let ids = &snapshot.participants;
            if ids.is_empty() {
                return;
            }
            let shown = ids.iter().position(|id| Some(id.as_str()) == self.latency_server());
            let next = ids[shown.map_or(0, |index| index + 1) % ids.len()].clone();
            let focus = snapshot.latency_focus.clone();
            self.latency_pick = (Some(&next) != focus.as_ref()).then_some(next);
        } else if self.running() && STOP.matches(name) {
            (self.stop_prompt, self.notice) = (true, "Stop the test? esc confirms, any other key continues.".into());
        } else if finished && SETUP.matches(name) {
            (self.live, self.row, self.body, self.notice) = (false, 0, 0, String::new());
            self.recheck_soon();
        } else if finished && RUN_AGAIN.matches(name) {
            self.start(commands);
        }
    }

    /// Go's handleSetupKey.
    fn setup_key(&mut self, name: &str, commands: &mpsc::Sender<Command>) {
        let row = self.current();
        if self.reset_prompt && !(CHANGE.matches(name) && row == Setting::Reset) {
            (self.reset_prompt, self.notice) = (false, "Settings kept.".into());
            return;
        }
        match row.flag(&self.config) {
            _ if ROWS.matches(name) => self.navigate(name),
            _ if ADJUST.matches(name) => self.adjust(row, delta(name)),
            Some(on) if TOGGLE.matches(name) => self.set_flag(row, !on),
            _ if CHANGE.matches(name) => self.activate(row, commands),
            _ if START.matches(name) => self.start(commands),
            _ if RECHECK.matches(name) => self.recheck_soon(),
            _ if SERVERS.matches(name) => self.open_servers(),
            _ if AVAILABLE.matches(name) && self.can_use_available() => {
                self.config.servers = self.ready_servers().map(str::to_owned).collect();
                self.notice = "Using the available servers.".into();
                self.recheck_soon();
            }
            _ if AUTOMATIC.matches(name) => {
                let config = &mut self.config;
                (config.throughput_origin, config.throughput_protocol, config.throughput_transport) =
                    (None, None, None);
                (config.latency_origin, config.latency_transport) = (None, None);
                self.notice = "Automatic paths applied to every selected server.".into();
                self.recheck_soon();
            }
            _ => {}
        }
    }

    /// Go's startRun: a valid setup starts a run; the results it replaces stay until it starts.
    fn start(&mut self, commands: &mpsc::Sender<Command>) {
        if let Err(error) = self.config.validate() {
            self.notice = format!("{BLOCKED}: {error}.");
            let latency = Setting::Stage(Stage::Latency);
            self.row = self.rows().iter().position(|row| *row == latency).unwrap_or(0);
            return;
        }
        if self.send(Command::Run(self.config.clone()), commands) {
            let run = std::mem::replace(&mut self.run, Run::new(self.config.clone()));
            self.previous = (self.live && self.snapshot.started()).then(|| (self.snapshot.clone(), run));
            (self.live, self.starting, self.open_chooser) = (true, true, false);
            (self.stop_prompt, self.popup, self.edit) = (false, Popup::None, None);
            self.notice = "Checking paths before the test. Press esc to stop.".into();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
