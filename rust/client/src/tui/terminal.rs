//! The terminal's setup and restore, and the interface's event loop on it.
use super::{App, Effect, Exit, FRAME, chrome::Chrome, dialogs, theme};
use crate::{config::Config, controller::Controller, events::Events, text::Profile};
use crossterm::{
    event::{DisableBracketedPaste, EnableBracketedPaste, Event as Input, EventStream, MouseEventKind},
    execute,
    terminal::{Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use futures_util::StreamExt;
use graphite_meter_net::Pool;
use graphite_meter_proto::origin::Origin;
use ratatui_core::{buffer::Buffer, terminal::Terminal};
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
pub async fn interactive(
    config: Config,
    kept: Option<Origin>,
    runtimes: Arc<Pool>,
    mut signals: UnboundedReceiver<u8>,
) -> io::Result<Exit> {
    let profile = theme::profile(true, |name| std::env::var(name).ok());
    let _session = Session::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(BufWriter::with_capacity(1 << 16, io::stdout())))?;
    terminal.hide_cursor()?;
    let (events, mut received) = Events::channel();
    let mut controller = Controller::new(true, runtimes, events);
    let mut input = pin!(EventStream::new().filter_map(|input| std::future::ready(input.ok())));
    let (mut app, mut shown) = (App::new(config, kept, profile, Instant::now()), None);
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
                // A pointer move repaints only when it reaches another pressable region.
                let moved = matches!(&input, Some(Input::Mouse(mouse)) if matches!(mouse.kind, MouseEventKind::Moved));
                let hovered = app.hovered();
                let effects = input.map_or_else(|| vec![Effect::Quit], |input| app.input(input, Instant::now()));
                let paint = !moved || app.hovered() != hovered;
                changed |= paint;
                (effects, paint)
            }
            _ = frames.tick() => {
                // While anything animates, and once after, so its last state shows.
                let animated = app.animating();
                let effects = app.tick(Instant::now());
                (effects, std::mem::take(&mut changed) || animated || app.animating())
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
                // A few bytes on local disk, written in order, so turning remembering off never loses to an earlier on.
                Effect::Remember(origin) => {
                    if let Some(path) = crate::config::kept_server() {
                        crate::config::remember(&path, &origin);
                    }
                }
                Effect::Forget => {
                    if let Some(path) = crate::config::kept_server() {
                        crate::config::forget(&path);
                    }
                }
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

/// Draws a frame: the changed cells and chrome in one write, a synchronized update where the terminal holds one. An
/// unchanged frame writes nothing, so the terminal has nothing to repaint.
fn draw(terminal: &mut Screen, app: &mut App, shown: &mut Option<(Buffer, Chrome)>) -> io::Result<()> {
    terminal.autoresize()?;
    let mut frame = terminal.get_frame();
    let cells = frame.buffer_mut();
    let drawn = app.draw(cells, Instant::now());
    let unchanged = shown.as_ref().is_some_and(|(last, _)| last == cells);
    let bytes = drawn.bytes(shown.as_ref().map(|(_, chrome)| chrome), !unchanged, app.profile);
    if unchanged && bytes.is_empty() {
        cells.reset();
        return Ok(());
    }
    let cells = cells.clone();
    terminal.backend_mut().write_all(b"\x1b[?2026h")?;
    terminal.flush()?;
    terminal.swap_buffers();
    let out = terminal.backend_mut();
    out.write_all(&bytes)?;
    out.write_all(b"\x1b[?2026l")?;
    out.flush()?;
    *shown = Some((cells, drawn));
    Ok(())
}

/// Pushes the window title and reports every pointer move, button and wheel turn in SGR form, so what the pointer rests
/// on lights up.
const ENTER: &[u8] = b"\x1b[22;0t\x1b[?1003h\x1b[?1006h";
/// Undoes `ENTER`, putting back the pushed title.
const LEAVE: &[u8] = b"\x1b[?1006l\x1b[?1003l\x1b[23;0t";

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
    let _ = io::stdout().write_all(&[Chrome::default().bytes(None, true, Profile::Plain), LEAVE.to_vec()].concat());
    let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}
