//! The terminal's setup and restore, and the event loop over any `Terminal` backend.
use super::{App, Effect, Exit, FRAME, chrome::Chrome, dialogs, theme};
use crate::{
    config::Config,
    controller::Controller,
    events::{Event, Events},
    text::Profile,
};
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste, Event as Input, EventStream},
    execute,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::{Stream, StreamExt};
use graphite_meter_net::Pool;
use ratatui_core::{backend::Backend, terminal::Terminal};
use std::{
    io::{self, Write},
    pin::pin,
    sync::Arc,
    time::Instant,
};
use tokio::sync::mpsc::UnboundedReceiver;

/// Runs the interface on this terminal until it quits; `signals` brings each caught signal's status.
pub async fn interactive(config: Config, runtimes: Arc<Pool>, signals: UnboundedReceiver<u8>) -> io::Result<Exit> {
    let profile = theme::profile(true, |name| std::env::var(name).ok());
    let _session = Session::enter()?;
    let mut terminal = Terminal::new(ratatui_crossterm::CrosstermBackend::new(io::stdout()))?;
    let (events, received) = Events::channel();
    let controller = Controller::new(true, runtimes, events);
    let input = EventStream::new().filter_map(|input| std::future::ready(input.ok()));
    let app = App::new(config, profile, Instant::now());
    run(&mut terminal, app, controller, received, input, signals, io::stdout()).await
}

/// Draws `app` and feeds it events, input, signals and ticks until it quits, writing its chrome to `chrome` after each
/// draw.
pub async fn run<B: Backend>(
    terminal: &mut Terminal<B>,
    mut app: App,
    mut controller: Controller,
    mut events: UnboundedReceiver<Event>,
    input: impl Stream<Item = Input>,
    mut signals: UnboundedReceiver<u8>,
    mut chrome: impl Write,
) -> Result<Exit, B::Error> {
    let (mut input, mut shown) = (pin!(input), None);
    let mut frames = tokio::time::interval(FRAME);
    frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut changed = true;
    loop {
        let effects = tokio::select! {
            Some(event) = events.recv() => {
                changed = true;
                app.event(&event, Instant::now())
            }
            input = input.next() => {
                changed = true;
                input.map_or_else(|| vec![Effect::Quit], |input| app.input(input, Instant::now()))
            }
            _ = frames.tick() => {
                let now = Instant::now();
                if std::mem::take(&mut changed) || app.animating() {
                    let mut drawn = Chrome::default();
                    terminal.draw(|frame| drawn = app.draw(frame.buffer_mut(), now))?;
                    let bytes = drawn.bytes(shown.as_ref(), app.profile);
                    let _ = chrome.write_all(&bytes).and_then(|()| chrome.flush());
                    shown = Some(drawn);
                }
                app.tick(now)
            }
            Some(status) = signals.recv() => {
                changed = true;
                app.interrupt(status)
            }
        };
        for effect in effects {
            match effect {
                Effect::Command(command) => controller.command(*command),
                Effect::OpenBrowser(url) => dialogs::browse(&url),
                Effect::Quit => return Ok(app.exit()),
            }
        }
    }
}

/// Pushes the window title and reports mouse buttons and the wheel, without plain moves, in SGR form.
const ENTER: &[u8] = b"\x1b[22;0t\x1b[?1002h\x1b[?1006h";
/// Undoes `ENTER`, putting back the pushed title.
const LEAVE: &[u8] = b"\x1b[?1006l\x1b[?1002l\x1b[23;0t";

/// The terminal in raw mode on the alternate screen, reporting the mouse and pastes and asked for its background;
/// dropping it clears the chrome, restores the terminal and puts back the panic hook it replaced.
struct Session {
    hook: Arc<Hook>,
}

type Hook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;

impl Session {
    fn enter() -> io::Result<Self> {
        let (hook, interface) = (Arc::new(std::panic::take_hook()), std::thread::current().id());
        let previous = hook.clone();
        // A panic on the interface's thread ends it; elsewhere the run ends as failed and the interface stays.
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == interface {
                restore();
            }
            previous(info);
        }));
        let session = Self { hook };
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        io::stdout().write_all(ENTER)?;
        let _ = execute!(io::stdout(), EnableBracketedPaste);
        // The answer arrives among the keys; the clear wipes what a terminal that ignores the query shows.
        io::stdout().write_all(theme::QUERY)?;
        execute!(io::stdout(), Clear(ClearType::All))?;
        Ok(session)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        restore();
        if !std::thread::panicking() {
            let hook = self.hook.clone();
            std::panic::set_hook(Box::new(move |info| hook(info)));
        }
    }
}

fn restore() {
    let _ = io::stdout().write_all(&[Chrome::default().bytes(None, Profile::Plain), LEAVE.to_vec()].concat());
    let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}
