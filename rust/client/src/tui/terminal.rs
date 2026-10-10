//! The terminal's setup and restore, and the interface's event loop on it.
use super::{App, Effect, Exit, FRAME, chrome::Chrome, dialogs, theme};
use crate::{config::Config, controller::Controller, events::Events, text::Profile};
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste, EventStream},
    execute,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use graphite_meter_net::Pool;
use ratatui_core::terminal::Terminal;
use ratatui_crossterm::CrosstermBackend;
use std::{
    io::{self, BufWriter, Write},
    pin::pin,
    sync::Arc,
    time::Instant,
};
use tokio::sync::mpsc::UnboundedReceiver;

/// Runs the interface until it quits, fed events, input, ticks and `signals`' statuses. Input paints at once; measurement
/// events wait for the next frame, so a burst of them costs one draw.
pub async fn interactive(config: Config, runtimes: Arc<Pool>, mut signals: UnboundedReceiver<u8>) -> io::Result<Exit> {
    let profile = theme::profile(true, |name| std::env::var(name).ok());
    let _session = Session::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(BufWriter::with_capacity(1 << 16, io::stdout())))?;
    let (events, mut received) = Events::channel();
    let mut controller = Controller::new(true, runtimes, events);
    let mut input = pin!(EventStream::new().filter_map(|input| std::future::ready(input.ok())));
    let (mut app, mut shown) = (App::new(config, profile, Instant::now()), None);
    let mut frames = tokio::time::interval(FRAME);
    frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut changed = true;
    loop {
        let (effects, paint) = tokio::select! {
            Some(event) = received.recv() => {
                changed = true;
                (app.event(&event, Instant::now()), false)
            }
            input = input.next() => {
                changed = true;
                (input.map_or_else(|| vec![Effect::Quit], |input| app.input(input, Instant::now())), true)
            }
            _ = frames.tick() => {
                let effects = app.tick(Instant::now());
                (effects, std::mem::take(&mut changed) || app.animating())
            }
            Some(status) = signals.recv() => {
                changed = true;
                (app.signal(status), true)
            }
        };
        for effect in effects {
            match effect {
                Effect::Command(command) => controller.command(*command),
                Effect::OpenBrowser(url) => dialogs::browse(&url),
                Effect::Quit => return Ok(app.exit()),
            }
        }
        if paint {
            changed = false;
            draw(&mut terminal, &mut app, &mut shown)?;
        }
    }
}

type Screen = Terminal<CrosstermBackend<BufWriter<io::Stdout>>>;

/// Draws one frame and the chrome after it as a synchronized update, so a terminal never shows half of it.
fn draw(terminal: &mut Screen, app: &mut App, shown: &mut Option<Chrome>) -> io::Result<()> {
    terminal.backend_mut().write_all(b"\x1b[?2026h")?;
    let mut drawn = Chrome::default();
    let now = Instant::now();
    terminal.draw(|frame| drawn = app.draw(frame.buffer_mut(), now))?;
    let bytes = drawn.bytes(shown.as_ref(), app.profile);
    let out = terminal.backend_mut();
    out.write_all(&bytes)?;
    out.write_all(b"\x1b[?2026l")?;
    out.flush()?;
    *shown = Some(drawn);
    Ok(())
}

/// Pushes the window title and reports mouse buttons and the wheel, without plain moves, in SGR form.
const ENTER: &[u8] = b"\x1b[22;0t\x1b[?1002h\x1b[?1006h";
/// Undoes `ENTER`, putting back the pushed title.
const LEAVE: &[u8] = b"\x1b[?1006l\x1b[?1002l\x1b[23;0t";

/// The terminal raw on the alternate screen with mouse, pastes and a background query; dropping it restores it all.
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
