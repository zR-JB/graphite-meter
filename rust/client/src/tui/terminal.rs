//! The terminal's setup and restore, and the interface's event loop on it.
use super::{
    App, Effect, Exit, FRAME,
    chrome::{self, Chrome, Progress},
    dialogs,
};
use crate::{
    config::Config,
    controller::Controller,
    events::Events,
    style::{self, Palette},
    text::Profile,
};
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
    io::{self, BufWriter, IsTerminal, Write},
    pin::pin,
    sync::Arc,
    time::Instant,
};
use tokio::sync::mpsc::UnboundedReceiver;

/// Runs the interface until it quits; a burst of measurement events costs one draw at the next frame.
pub async fn interactive(
    config: Config,
    kept: Option<Origin>,
    runtimes: Arc<Pool>,
    mut signals: UnboundedReceiver<u8>,
) -> io::Result<Exit> {
    let var = |name: &str| std::env::var(name).ok();
    let (profile, progress) = (style::profile(true, var), chrome::progress(var));
    let _session = Session::enter(progress)?;
    // The answer is read before the input stream starts, which would take it for keys.
    let asked = io::stdin()
        .is_terminal()
        .then(|| tokio::task::spawn_blocking(|| style::ask(style::ANSWER)));
    let background = match asked {
        Some(asked) => asked.await.ok().flatten(),
        None => None,
    };
    // The clear wipes what a terminal that ignores the query shows.
    execute!(io::stdout(), Clear(ClearType::All))?;
    let palette = background.map_or_else(|| Palette::new(true), Palette::over);
    let mut terminal = Terminal::new(CrosstermBackend::new(BufWriter::with_capacity(1 << 16, io::stdout())))?;
    terminal.hide_cursor()?;
    let (events, mut received) = Events::channel();
    let mut controller = Controller::new(true, runtimes, events);
    // A read error ends the input, which quits, instead of polling a broken terminal again.
    let input = EventStream::new().take_while(|input| std::future::ready(input.is_ok()));
    let mut input = pin!(input.filter_map(|input| std::future::ready(input.ok())));
    let (mut app, mut shown) = (App::new(config, kept, (profile, palette), Instant::now()), None);
    let mut frames = tokio::time::interval(FRAME);
    frames.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let (mut changed, mut resized) = (true, true);
    loop {
        let (effects, paint) = tokio::select! {
            Some(event) = received.recv() => {
                changed = true;
                (app.event(&event, Instant::now()), false)
            }
            input = input.next() => {
                // A pointer move repaints only when it reaches another pressable region.
                let moved = matches!(&input, Some(Input::Mouse(mouse)) if matches!(mouse.kind, MouseEventKind::Moved));
                resized |= matches!(&input, Some(Input::Resize(..)));
                let hovered = app.hovered();
                let effects = input.map_or_else(|| vec![Effect::Quit], |input| app.input(input, Instant::now()));
                let paint = !moved || app.hovered() != hovered;
                changed |= paint;
                (effects, paint)
            }
            // Idle, nothing ticks: the frame clock runs only while there is something to paint or a check is due.
            _ = frames.tick(), if changed || app.waiting() => {
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
            // The terminal's size is asked for only after it reported a resize.
            if std::mem::take(&mut resized) {
                terminal.autoresize()?;
            }
            draw(&mut terminal, &mut app, &mut shown, progress)?;
        }
    }
}

type Screen = Terminal<CrosstermBackend<BufWriter<io::Stdout>>>;

/// Draws a frame: the changed cells and chrome in one write, a synchronized update where the terminal holds one, the
/// progress bar where it draws one. An unchanged frame writes nothing, so the terminal has nothing to repaint.
fn draw(terminal: &mut Screen, app: &mut App, shown: &mut Option<(Buffer, Chrome)>, progress: bool) -> io::Result<()> {
    let mut frame = terminal.get_frame();
    let cells = frame.buffer_mut();
    let mut drawn = app.draw(cells, Instant::now());
    if !progress {
        drawn.progress = Progress::None;
    }
    let unchanged = shown.as_ref().is_some_and(|(last, _)| last == cells);
    let bytes = drawn.bytes(shown.as_ref().map(|(_, chrome)| chrome), !unchanged, app.profile);
    if unchanged && bytes.is_empty() {
        cells.reset();
        return Ok(());
    }
    // The last frame's cells are overwritten in place, so a frame allocates no screen-sized buffer.
    match shown {
        Some((last, chrome)) => {
            last.area = cells.area;
            last.content.clone_from(&cells.content);
            *chrome = drawn;
        }
        None => *shown = Some((cells.clone(), drawn)),
    }
    terminal.backend_mut().write_all(b"\x1b[?2026h")?;
    terminal.flush()?;
    terminal.swap_buffers();
    let out = terminal.backend_mut();
    out.write_all(&bytes)?;
    out.write_all(b"\x1b[?2026l")?;
    out.flush()?;
    Ok(())
}

/// Pushes the window title and reports every pointer move in SGR form, so what the pointer rests on lights up.
const ENTER: &[u8] = b"\x1b[22;0t\x1b[?1003h\x1b[?1006h";
/// Undoes `ENTER`, putting back the pushed title.
const LEAVE: &[u8] = b"\x1b[?1006l\x1b[?1003l\x1b[23;0t";

/// The terminal raw on the alternate screen with mouse and pastes; dropping it restores it all, the progress bar
/// cleared where the terminal draws one.
struct Session {
    hook: Arc<Hook>,
    progress: bool,
}

type Hook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;

impl Session {
    fn enter(progress: bool) -> io::Result<Self> {
        let (hook, interface) = (Arc::new(std::panic::take_hook()), std::thread::current().id());
        let previous = hook.clone();
        // A panic on the interface's thread ends it; elsewhere the run ends as failed and the interface stays.
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().id() == interface {
                restore(progress);
            }
            previous(info);
        }));
        let session = Self { hook, progress };
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        io::stdout().write_all(ENTER)?;
        let _ = execute!(io::stdout(), EnableBracketedPaste);
        Ok(session)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        restore(self.progress);
        if !std::thread::panicking() {
            let hook = self.hook.clone();
            std::panic::set_hook(Box::new(move |info| hook(info)));
        }
    }
}

fn restore(progress: bool) {
    let mut out = Chrome::default().bytes(None, true, Profile::Plain);
    if progress {
        out.extend_from_slice(Progress::CLEAR);
    }
    out.extend_from_slice(LEAVE);
    let _ = io::stdout().write_all(&out);
    let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}
