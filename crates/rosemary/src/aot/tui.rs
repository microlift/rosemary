//! A live status panel for `rosemary aot` on a terminal.
//!
//! Redrawn in place on standard error below whatever was printed before it; the
//! last frame stays behind as a summary. [`render`] is pure, so what is drawn
//! does not depend on a terminal being there.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write as _};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use super::entries::Source;
use super::progress::{
    Activity, Board, BoardView, EventKind, ImageState, ImageView, Phase, WorkerView,
};

/// How often the panel is redrawn.
const TICK: Duration = Duration::from_millis(100);
/// How often each thread's timeline gains a sample.
const SAMPLE: Duration = Duration::from_millis(250);
/// Samples of each thread's activity kept.
const HISTORY: usize = 96;
/// Throughput and time left are measured over this much of the recent past.
const RATE_WINDOW: Duration = Duration::from_secs(10);
/// Per-second throughput samples kept for the sparkline.
const THROUGHPUT_KEPT: usize = 60;
/// Samples the recent split of time across activities is measured over.
const SHARE_SAMPLES: usize = 20;
/// Assumed when the terminal will not say how large it is.
const FALLBACK_SIZE: Size = Size {
    width: 100,
    height: 40,
};
/// The `no-color.org` convention.
const NO_COLOR_ENV: &str = "NO_COLOR";

/// A region in flight longer than this is slow; longer than [`STUCK`], very slow.
const SLOW: Duration = Duration::from_secs(1);
const STUCK: Duration = Duration::from_secs(10);

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
/// Partial cells, an eighth wider each, filling from the left.
const EIGHTHS: [char; 7] = ['▏', '▎', '▍', '▌', '▋', '▊', '▉'];
/// Bars a sixteenth taller each, for sparklines; index 0 is nothing at all.
const LEVELS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// 256-colour palette indices.
mod palette {
    pub const ACCENT: u8 = 114;
    pub const HOT: u8 = 215;
    pub const HOT_DIM: u8 = 94;
    pub const COLD: u8 = 75;
    pub const BAD: u8 = 203;
    pub const WARN: u8 = 221;
    pub const GOOD: u8 = 114;
    pub const LIFT: u8 = 80;
    pub const COMPILE: u8 = 114;
    pub const WRITE: u8 = 177;
    pub const WAIT: u8 = 221;
    pub const COMMIT: u8 = 147;
    pub const MUTED: u8 = 245;
    pub const FAINT: u8 = 238;
}

/// The panel while it is being drawn.
pub struct Display {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

/// Whether standard error is a terminal the panel can be drawn on.
pub fn available() -> bool {
    std::io::stderr().is_terminal() && std::env::var("TERM").is_ok_and(|t| t != "dumb")
}

/// Starts drawing `board` on standard error; see [`available`].
pub fn start(board: Arc<Board>) -> Option<Display> {
    let color = std::env::var_os(NO_COLOR_ENV).is_none_or(|v| v.is_empty());
    let stop = Arc::new(AtomicBool::new(false));
    let thread = std::thread::Builder::new()
        .name("aot-progress".into())
        .spawn({
            let stop = stop.clone();
            move || run(&board, &stop, color)
        })
        .ok()?;
    Some(Display {
        stop,
        thread: Some(thread),
    })
}

impl Display {
    /// Draws the last frame and stops.
    pub fn finish(self) {}
}

impl Drop for Display {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

fn run(board: &Board, stop: &AtomicBool, color: bool) {
    let mut terminal = Terminal::default();
    let mut tracker = Tracker::default();
    let mut tick = 0;
    loop {
        let last = stop.load(Ordering::Acquire);
        let view = board.view();
        tracker.observe(&view);
        let size = terminal_size();
        let frame = render(&view, &tracker, size, tick, last);
        terminal.draw(&frame, size.width, color);
        if last {
            return;
        }
        tick += 1;
        std::thread::park_timeout(TICK);
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Size {
    pub width: usize,
    pub height: usize,
}

fn terminal_size() -> Size {
    // SAFETY: `TIOCGWINSZ` only fills in the `winsize` it is handed.
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    let known = unsafe { libc::ioctl(libc::STDERR_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
    match known && size.ws_col > 0 && size.ws_row > 0 {
        true => Size {
            width: size.ws_col.into(),
            height: size.ws_row.into(),
        },
        false => FALLBACK_SIZE,
    }
}

/// What was drawn last, so the next frame can replace it.
#[derive(Default)]
struct Terminal {
    /// The width of each line of the last frame.
    drawn: Vec<usize>,
}

impl Terminal {
    fn draw(&mut self, frame: &[Line], width: usize, color: bool) {
        // A line exactly as wide as the terminal wraps on some terminals and not
        // others; one column short never does.
        let room = width.saturating_sub(1).max(1);
        let mut out = String::from("\x1b[?2026h");
        // Lines a narrower terminal has since reflowed take more rows than they did.
        let rows: usize = self.drawn.iter().map(|&w| w.div_ceil(width).max(1)).sum();
        match rows {
            0 => out.push('\r'),
            rows => {
                let _ = write!(out, "\x1b[{rows}F");
            }
        }
        self.drawn.clear();
        for line in frame {
            self.drawn.push(line.write(&mut out, room, color));
            out.push_str("\x1b[0m\x1b[K\n");
        }
        out.push_str("\x1b[J\x1b[?2026l");
        let mut stderr = std::io::stderr().lock();
        let _ = stderr.write_all(out.as_bytes());
        let _ = stderr.flush();
    }
}

/// What the panel remembers between frames.
#[derive(Default)]
pub struct Tracker {
    /// `(elapsed, regions finished)`, over [`RATE_WINDOW`].
    finished: VecDeque<(Duration, usize)>,
    /// Regions finished per second, one sample a second.
    throughput: VecDeque<f64>,
    last_second: Option<(Duration, usize)>,
    last_sample: Duration,
    workers: Vec<WorkerHistory>,
    /// Time spent per activity by every thread, per sample.
    shares: VecDeque<[Duration; Activity::COUNT]>,
}

#[derive(Default)]
struct WorkerHistory {
    sampled: [Duration; Activity::COUNT],
    samples: VecDeque<Sample>,
}

#[derive(Copy, Clone, Debug)]
struct Sample {
    /// The share of the sample spent doing anything but waiting for work.
    busy: f64,
    /// What most of that was.
    doing: Activity,
}

impl Tracker {
    pub fn observe(&mut self, view: &BoardView) {
        let now = view.elapsed;
        let (finished, _) = view.finished();
        self.finished.push_back((now, finished));
        while self.finished.len() > 2
            && self
                .finished
                .front()
                .is_some_and(|&(at, _)| now.saturating_sub(at) > RATE_WINDOW)
        {
            self.finished.pop_front();
        }
        match self.last_second {
            Some((at, count)) if now.saturating_sub(at) >= Duration::from_secs(1) => {
                let seconds = now.saturating_sub(at).as_secs_f64();
                if self.throughput.len() == THROUGHPUT_KEPT {
                    self.throughput.pop_front();
                }
                self.throughput
                    .push_back(finished.saturating_sub(count) as f64 / seconds);
                self.last_second = Some((now, finished));
            }
            Some(_) => {}
            None => self.last_second = Some((now, finished)),
        }

        if self.workers.len() != view.workers.len() {
            self.workers = view
                .workers
                .iter()
                .map(|_| WorkerHistory::default())
                .collect();
            self.shares.clear();
        }
        if now.saturating_sub(self.last_sample) < SAMPLE {
            return;
        }
        self.last_sample = now;
        let mut share = [Duration::ZERO; Activity::COUNT];
        for (history, worker) in self.workers.iter_mut().zip(&view.workers) {
            let delta: [Duration; Activity::COUNT] =
                std::array::from_fn(|n| worker.spent[n].saturating_sub(history.sampled[n]));
            history.sampled = worker.spent;
            let total: Duration = delta.iter().sum();
            if total.is_zero() {
                continue;
            }
            for (sum, spent) in share.iter_mut().zip(delta) {
                *sum += spent;
            }
            let idle = delta[Activity::Idle as usize];
            let doing = Activity::ALL[1..]
                .iter()
                .copied()
                .max_by_key(|&a| delta[a as usize])
                .unwrap_or(Activity::Idle);
            if history.samples.len() == HISTORY {
                history.samples.pop_front();
            }
            history.samples.push_back(Sample {
                busy: 1.0 - idle.as_secs_f64() / total.as_secs_f64(),
                doing,
            });
        }
        if self.shares.len() == SHARE_SAMPLES {
            self.shares.pop_front();
        }
        self.shares.push_back(share);
    }

    /// Regions finished per second, lately.
    fn rate(&self) -> Option<f64> {
        let (&(from, a), &(to, b)) = (self.finished.front()?, self.finished.back()?);
        let seconds = to.saturating_sub(from).as_secs_f64();
        (seconds >= 1.0).then(|| b.saturating_sub(a) as f64 / seconds)
    }

    /// The time `remaining` regions will take at the recent rate.
    fn time_for(&self, remaining: usize) -> Option<Duration> {
        let rate = self.rate().filter(|&rate| rate > 0.0)?;
        Some(Duration::from_secs_f64(remaining as f64 / rate))
    }

    /// The share of recent time all threads spent on each activity.
    fn recent_share(&self) -> [Duration; Activity::COUNT] {
        let mut sum = [Duration::ZERO; Activity::COUNT];
        for sample in &self.shares {
            for (total, spent) in sum.iter_mut().zip(sample) {
                *total += *spent;
            }
        }
        sum
    }
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
struct Style {
    fg: Option<u8>,
    bold: bool,
    dim: bool,
}

const PLAIN: Style = Style {
    fg: None,
    bold: false,
    dim: false,
};

const fn fg(color: u8) -> Style {
    Style {
        fg: Some(color),
        bold: false,
        dim: false,
    }
}

const fn bold(color: u8) -> Style {
    Style {
        fg: Some(color),
        bold: true,
        dim: false,
    }
}

const MUTED: Style = fg(palette::MUTED);
const FAINT: Style = fg(palette::FAINT);
const HEADING: Style = Style {
    fg: None,
    bold: true,
    dim: false,
};

/// One line of the panel.
#[derive(Clone, Debug, Default)]
pub struct Line {
    spans: Vec<(Style, String)>,
}

impl Line {
    fn new() -> Self {
        Line::default()
    }

    fn push(&mut self, style: Style, text: impl Into<String>) -> &mut Self {
        let text = text.into();
        match self.spans.last_mut() {
            Some((last, joined)) if *last == style => joined.push_str(&text),
            _ => self.spans.push((style, text)),
        }
        self
    }

    fn push_char(&mut self, style: Style, c: char) -> &mut Self {
        match self.spans.last_mut() {
            Some((last, joined)) if *last == style => joined.push(c),
            _ => self.spans.push((style, c.to_string())),
        }
        self
    }

    fn width(&self) -> usize {
        self.spans.iter().map(|(_, text)| text_width(text)).sum()
    }

    /// Pads with spaces to `width` columns.
    fn pad(&mut self, width: usize) -> &mut Self {
        let have = self.width();
        if have < width {
            self.push(PLAIN, " ".repeat(width - have));
        }
        self
    }

    /// Appends `right` flush against column `width`, if it fits after a gap.
    fn right(&mut self, width: usize, right: Line) -> &mut Self {
        let (have, wanted) = (self.width(), right.width());
        if have + wanted + 2 <= width {
            self.pad(width - wanted);
            self.spans.extend(right.spans);
        }
        self
    }

    /// Writes the line, cut to `width` columns, and returns how wide it was.
    fn write(&self, out: &mut String, width: usize, color: bool) -> usize {
        let cut = self.width() > width;
        let room = if cut { width.saturating_sub(1) } else { width };
        let mut used = 0;
        for (style, text) in &self.spans {
            if used >= room {
                break;
            }
            if color {
                style.open(out);
            }
            for c in text.chars() {
                let c = if c.is_control() { '?' } else { c };
                let w = char_width(c);
                if used + w > room {
                    break;
                }
                out.push(c);
                used += w;
            }
            if color {
                out.push_str("\x1b[0m");
            }
        }
        if cut {
            out.push('…');
            used += 1;
        }
        used
    }

    /// The text alone, for tests.
    pub fn text(&self) -> String {
        self.spans.iter().map(|(_, text)| text.as_str()).collect()
    }
}

impl Style {
    fn open(&self, out: &mut String) {
        if self.bold {
            out.push_str("\x1b[1m");
        }
        if self.dim {
            out.push_str("\x1b[2m");
        }
        if let Some(color) = self.fg {
            let _ = write!(out, "\x1b[38;5;{color}m");
        }
    }
}

/// Columns `c` takes: two for East Asian wide characters, none for combining
/// marks. Enough for names; not a full Unicode width table.
fn char_width(c: char) -> usize {
    match u32::from(c) {
        0x0300..=0x036F | 0x200B..=0x200F | 0xFE00..=0xFE0F => 0,
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE4F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1F64F
        | 0x1F900..=0x1F9FF
        | 0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

fn text_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

/// Draws the panel for `view` in at most `size`, as of redraw number `tick`;
/// `last` for the frame left behind.
pub fn render(view: &BoardView, tracker: &Tracker, size: Size, tick: u64, last: bool) -> Vec<Line> {
    let width = size.width.saturating_sub(1).max(20);
    let mut frame = Vec::new();
    frame.push(title(view, tracker, width, tick));
    frame.extend(overall(view, tracker, width));

    let discovery = discovery(view, width);
    let images = images(view, width, tick);
    let events = events(view, width, if last { 4 } else { 3 });
    let cores_full = cores(view, tracker, width, last, false);

    // Rows left after the head, a blank before each section, and a little of
    // everything; the rest goes to threads, then images, then events.
    let budget = size.height.saturating_sub(2).max(frame.len() + 4);
    let mut fixed = frame.len() + discovery.len();
    if !discovery.is_empty() {
        fixed += 1;
    }
    let image_min = images.len().min(4);
    let event_rows = |n: usize| if n == 0 { 0 } else { n + 1 };
    let cores = match fixed + 1 + image_min + 1 + cores_full.len() <= budget {
        true => cores_full,
        false => cores(view, tracker, width, last, true),
    };
    if !cores.is_empty() {
        fixed += 1 + cores.len();
    }
    let image_rows = budget.saturating_sub(fixed + 1).min(images.len());
    let event_count = budget
        .saturating_sub(fixed + 1 + image_rows)
        .saturating_sub(1)
        .min(events.len());

    if !discovery.is_empty() {
        frame.push(Line::new());
        frame.extend(discovery);
    }
    if image_rows > 0 {
        frame.push(Line::new());
        frame.extend(fit(images, image_rows));
    }
    if !cores.is_empty() {
        frame.push(Line::new());
        frame.extend(cores);
    }
    if event_rows(event_count) > 0 {
        frame.push(Line::new());
        frame.extend(events.into_iter().rev().take(event_count).rev());
    }
    frame
}

/// The first `rows` of `lines`, ending in a note of how many did not fit.
fn fit(mut lines: Vec<Line>, rows: usize) -> Vec<Line> {
    if lines.len() <= rows {
        return lines;
    }
    let hidden = lines.len() - rows + 1;
    lines.truncate(rows.saturating_sub(1));
    let mut more = Line::new();
    more.push(MUTED, format!("  … {hidden} more"));
    lines.push(more);
    lines
}

fn title(view: &BoardView, tracker: &Tracker, width: usize, tick: u64) -> Line {
    let failed = view.images.iter().any(|i| i.state == ImageState::Failed);
    let stopped = view.images.iter().any(|i| i.state == ImageState::Stopped);
    let mut line = Line::new();
    match view.phase {
        Phase::Done if failed => line.push(bold(palette::BAD), "✗"),
        Phase::Done if stopped => line.push(bold(palette::WARN), "◐"),
        Phase::Done => line.push(bold(palette::GOOD), "✓"),
        _ => line.push_char(bold(palette::ACCENT), spinner(tick)),
    };
    line.push(PLAIN, " ")
        .push(bold(palette::ACCENT), "rosemary aot")
        .push(MUTED, " · ")
        .push(HEADING, view.program.clone());
    if !view.subject.is_empty() && view.phase != Phase::Loading {
        line.push(MUTED, format!("  {}", view.subject));
    }

    let mut right = Line::new();
    let (phase, style) = match view.phase {
        Phase::Loading => ("loading", fg(palette::LIFT)),
        Phase::Discovering => ("discovering", fg(palette::LIFT)),
        Phase::Compiling => ("compiling", fg(palette::COMPILE)),
        Phase::Finishing => ("committing", fg(palette::COMMIT)),
        Phase::Done => ("done", fg(palette::GOOD)),
    };
    right
        .push(style, phase)
        .push(MUTED, "  ")
        .push(PLAIN, clock(view.elapsed));
    if view.phase == Phase::Compiling {
        let (planned, _) = view.planned();
        let (finished, _) = view.finished();
        match tracker.time_for(planned.saturating_sub(finished)) {
            Some(left) => right
                .push(MUTED, "  ~")
                .push(PLAIN, clock(left))
                .push(MUTED, " left"),
            None => right.push(MUTED, "  estimating…"),
        };
    }
    if let Some(deadline) = view.deadline
        && view.phase != Phase::Done
    {
        match view.deadline_reached {
            true => right.push(fg(palette::WARN), "  deadline reached"),
            false => right.push(MUTED, "  deadline ").push(
                fg(palette::WARN),
                clock(deadline.saturating_sub(view.elapsed)),
            ),
        };
    }
    line.right(width, right);
    line
}

/// The overall bar and the line of figures under it.
fn overall(view: &BoardView, tracker: &Tracker, width: usize) -> Vec<Line> {
    let (planned, hot) = view.planned();
    let (finished, hot_finished) = view.finished();
    let mut bar_line = Line::new();
    if matches!(view.phase, Phase::Loading | Phase::Discovering) {
        let entries: usize = view.images.iter().map(|i| i.entries).sum();
        let images = view
            .images
            .iter()
            .filter(|i| !i.state.discovering())
            .count();
        bar_line.push(MUTED, "  ");
        match view.phase {
            Phase::Loading => bar_line.push(MUTED, "loading and linking the program…"),
            _ => bar_line
                .push(PLAIN, group(entries))
                .push(MUTED, " entries in ")
                .push(PLAIN, images.to_string())
                .push(MUTED, format!(" image{} → ", plural(images)))
                .push(PLAIN, group(planned))
                .push(MUTED, " regions planned"),
        };
        return vec![bar_line];
    }

    let bar_width = width.saturating_sub(34).clamp(10, 72);
    let total = planned.max(1) as f64;
    let cells = bar_width as f64;
    let hot_end = hot as f64 / total * cells;
    let cold_finished = finished.saturating_sub(hot_finished);
    let fills = [
        Fill {
            from: 0.0,
            to: hot_finished as f64 / total * cells,
            color: palette::HOT,
        },
        Fill {
            from: hot_end,
            to: hot_end + cold_finished as f64 / total * cells,
            color: palette::COLD,
        },
    ];
    bar_line.push(PLAIN, " ");
    bar(&mut bar_line, bar_width, &fills, |cell| {
        match (cell as f64) < hot_end {
            true => palette::HOT_DIM,
            false => palette::FAINT,
        }
    });
    let percent = finished as f64 / total * 100.0;
    bar_line
        .push(HEADING, format!(" {percent:5.1}%"))
        .push(MUTED, "  ")
        .push(PLAIN, group(finished))
        .push(MUTED, " / ")
        .push(PLAIN, group(planned))
        .push(MUTED, " regions");

    let mut figures = Line::new();
    figures.push(PLAIN, "  ");
    let committed = view.images.iter().filter(|i| i.hot_committed).count();
    let with_hot = view
        .images
        .iter()
        .filter(|i| i.hot > 0 || i.hot_committed)
        .count();
    figures
        .push(fg(palette::HOT), "■ ")
        .push(MUTED, "start-up ");
    if hot_finished >= hot {
        figures.push(fg(palette::GOOD), "✓ compiled");
    } else {
        figures.push(
            PLAIN,
            format!("{:.0}%", hot_finished as f64 / hot.max(1) as f64 * 100.0),
        );
        if let Some(left) = tracker.time_for(hot - hot_finished) {
            figures.push(MUTED, format!(" ~{}", clock(left)));
        }
    }
    figures.push(MUTED, format!(" ({committed}/{with_hot} committed)"));
    figures
        .push(MUTED, "   ")
        .push(fg(palette::COLD), "■ ")
        .push(MUTED, "rest");
    figures.push(MUTED, "   ");
    let rate = tracker.rate().unwrap_or(0.0);
    figures
        .push(HEADING, short(rate as usize))
        .push(MUTED, " regions/s ");
    sparkline(&mut figures, tracker.throughput.iter().copied(), 16, |_| {
        palette::ACCENT
    });
    let code: u64 = view.images.iter().map(|i| i.code_bytes).sum();
    figures
        .push(MUTED, "   ")
        .push(PLAIN, bytes(code))
        .push(MUTED, " of code");
    let refused: usize = view.images.iter().map(|i| i.refused).sum();
    if refused > 0 {
        figures
            .push(MUTED, "   ")
            .push(fg(palette::BAD), group(refused))
            .push(MUTED, " refused");
    }
    vec![bar_line, figures]
}

/// The images still being read or planned.
fn discovery(view: &BoardView, width: usize) -> Vec<Line> {
    let Some(image) = view.images.iter().find(|i| i.state.discovering()) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let mut head = Line::new();
    let doing = match image.state {
        ImageState::Reading => "reading and hashing",
        ImageState::Discovering => "finding entry points",
        _ => "choosing regions",
    };
    head.push(HEADING, "DISCOVERY  ")
        .push(PLAIN, image.name.clone())
        .push(MUTED, format!("  {doing}"));
    lines.push(head);
    if image.state == ImageState::Planning {
        let mut found = Line::new();
        found.push(PLAIN, "  ");
        let mut first = true;
        for source in Source::ALL {
            let count = image.sources[source as usize];
            if count == 0 {
                continue;
            }
            if !first {
                found.push(FAINT, " · ");
            }
            first = false;
            found
                .push(fg(source_color(source)), short(count))
                .push(MUTED, format!(" {}", source_label(source)));
        }
        lines.push(found);

        let mut planning = Line::new();
        let bar_width = width.saturating_sub(60).clamp(10, 40);
        let done = image.examined as f64 / image.entries.max(1) as f64;
        planning.push(PLAIN, "  ");
        bar(
            &mut planning,
            bar_width,
            &[Fill {
                from: 0.0,
                to: done * bar_width as f64,
                color: palette::LIFT,
            }],
            |_| palette::FAINT,
        );
        planning
            .push(PLAIN, format!(" {}", group(image.examined)))
            .push(MUTED, format!(" / {} entries → ", group(image.entries)))
            .push(PLAIN, group(image.planned))
            .push(MUTED, " regions   at ")
            .push(fg(palette::LIFT), format!("{:#x}", image.cursor));
        lines.push(planning);
    }
    lines
}

fn images(view: &BoardView, width: usize, tick: u64) -> Vec<Line> {
    let shown: Vec<&ImageView> = {
        let mut shown: Vec<&ImageView> = view
            .images
            .iter()
            .filter(|i| i.state != ImageState::Skipped)
            .collect();
        shown.sort_by_key(|i| {
            (
                image_rank(i.state),
                std::cmp::Reverse(i.planned - i.finished().min(i.planned)),
            )
        });
        shown
    };
    let skipped: Vec<&ImageView> = view
        .images
        .iter()
        .filter(|i| i.state == ImageState::Skipped)
        .collect();
    if shown.is_empty() && skipped.is_empty() {
        return Vec::new();
    }

    let mut head = Line::new();
    head.push(HEADING, "IMAGES ")
        .push(MUTED, format!("{}", view.images.len()));
    for (state, label) in [
        (ImageState::Compiling, "compiling"),
        (ImageState::Queued, "queued"),
        (ImageState::Done, "done"),
        (ImageState::Stopped, "stopped"),
        (ImageState::Failed, "failed"),
    ] {
        let count = view.images.iter().filter(|i| i.state == state).count();
        if count > 0 {
            head.push(FAINT, " · ")
                .push(fg(state_color(state)), count.to_string())
                .push(MUTED, format!(" {label}"));
        }
    }
    if !skipped.is_empty() {
        head.push(FAINT, " · ")
            .push(MUTED, format!("{} skipped", skipped.len()));
    }

    let mut lines = vec![head];
    let name_width = shown
        .iter()
        .map(|i| text_width(&i.name))
        .max()
        .unwrap_or(0)
        .clamp(8, 28);
    let bar_width = width.saturating_sub(name_width + 62).clamp(8, 24);
    for image in shown {
        let mut line = Line::new();
        let (glyph, color) = match image.state {
            ImageState::Compiling => (spinner(tick), palette::ACCENT),
            ImageState::Done => ('✓', palette::GOOD),
            ImageState::Stopped => ('◐', palette::WARN),
            ImageState::Failed => ('✗', palette::BAD),
            ImageState::Queued => ('·', palette::MUTED),
            _ => ('◌', palette::LIFT),
        };
        line.push(PLAIN, "  ")
            .push_char(bold(color), glyph)
            .push(PLAIN, " ");
        line.push(PLAIN, truncate(&image.name, name_width))
            .pad(name_width + 5);
        if image.state == ImageState::Failed {
            line.push(fg(palette::BAD), image.note.clone().unwrap_or_default());
            lines.push(line);
            continue;
        }
        let total = image.planned.max(1) as f64;
        let cells = bar_width as f64;
        let hot_end = image.hot as f64 / total * cells;
        bar(
            &mut line,
            bar_width,
            &[
                Fill {
                    from: 0.0,
                    to: image.hot_finished as f64 / total * cells,
                    color: palette::HOT,
                },
                Fill {
                    from: hot_end,
                    to: hot_end
                        + image.finished().saturating_sub(image.hot_finished) as f64 / total
                            * cells,
                    color: palette::COLD,
                },
            ],
            |cell| match (cell as f64) < hot_end {
                true => palette::HOT_DIM,
                false => palette::FAINT,
            },
        );
        let percent = image.finished() as f64 / total * 100.0;
        line.push(PLAIN, format!(" {percent:>3.0}%"))
            .push(PLAIN, format!(" {:>9}", group(image.finished())))
            .push(MUTED, format!(" / {:<9}", group(image.planned)));
        match image.hot_committed {
            true => line.push(fg(palette::HOT), " ●"),
            false => line.push(FAINT, " ○"),
        };
        line.push(
            MUTED,
            format!(" {:>2} part{}", image.parts, plural(image.parts as usize)),
        )
        .push(PLAIN, format!(" {:>9}", bytes(image.code_bytes)));
        if image.refused > 0 {
            line.push(
                fg(palette::BAD),
                format!("  {} refused", short(image.refused)),
            );
        }
        lines.push(line);
    }
    if !skipped.is_empty() {
        let mut reasons: Vec<(String, usize)> = Vec::new();
        for image in &skipped {
            let reason = image.note.clone().unwrap_or_default();
            match reasons.iter_mut().find(|(r, _)| *r == reason) {
                Some((_, count)) => *count += 1,
                None => reasons.push((reason, 1)),
            }
        }
        let mut line = Line::new();
        line.push(MUTED, "  ⊘ skipped: ");
        for (n, (reason, count)) in reasons.iter().enumerate() {
            if n > 0 {
                line.push(FAINT, " · ");
            }
            line.push(MUTED, format!("{reason} ×{count}"));
        }
        lines.push(line);
    }
    lines
}

/// The compiler threads: how their time is split, and each one's region. `compact`
/// draws one cell a thread instead of a line, with only the slowest regions named.
fn cores(
    view: &BoardView,
    tracker: &Tracker,
    width: usize,
    last: bool,
    compact: bool,
) -> Vec<Line> {
    if view.workers.is_empty() {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let share = match last {
        // The whole build, once it is over; the recent past while it runs.
        true => {
            let mut sum = [Duration::ZERO; Activity::COUNT];
            for worker in &view.workers {
                for (total, spent) in sum.iter_mut().zip(worker.spent) {
                    *total += spent;
                }
            }
            sum
        }
        false => tracker.recent_share(),
    };
    let total: Duration = share.iter().sum();
    let busy = 1.0 - share[Activity::Idle as usize].as_secs_f64() / total.as_secs_f64().max(1e-9);

    let mut head = Line::new();
    head.push(HEADING, "CORES ")
        .push(PLAIN, view.workers.len().to_string())
        .push(MUTED, format!(" thread{}  ", plural(view.workers.len())))
        .push(HEADING, format!("{:.0}%", busy * 100.0))
        .push(MUTED, " busy  ");
    let order = [
        Activity::Lifting,
        Activity::Compiling,
        Activity::Writing,
        Activity::Committing,
        Activity::Waiting,
        Activity::Idle,
    ];
    stacked(
        &mut head,
        24,
        &order.map(|a| (share[a as usize], activity_color(a))),
    );
    for activity in order {
        let part = share[activity as usize].as_secs_f64() / total.as_secs_f64().max(1e-9);
        if part < 0.005 {
            continue;
        }
        head.push(PLAIN, "  ")
            .push(fg(activity_color(activity)), "■ ")
            .push(
                MUTED,
                format!("{} {:.0}%", activity_label(activity), part * 100.0),
            );
    }
    lines.push(head);

    if view.budget_used > 0 || view.deferred > 0 {
        let mut budget = Line::new();
        budget.push(MUTED, "  large regions ");
        let used = view.budget_used as f64 / view.budget_limit.max(1) as f64;
        bar(
            &mut budget,
            16,
            &[Fill {
                from: 0.0,
                to: used.min(1.0) * 16.0,
                color: palette::WAIT,
            }],
            |_| palette::FAINT,
        );
        budget
            .push(PLAIN, format!(" {}", short(view.budget_used)))
            .push(
                MUTED,
                format!(" / {} CLIF instructions", short(view.budget_limit)),
            );
        if view.deferred > 0 {
            budget
                .push(MUTED, " · ")
                .push(fg(palette::WAIT), view.deferred.to_string())
                .push(MUTED, " waiting for room");
        }
        lines.push(budget);
    }

    if last {
        // Where the regions went: one bar per thread, against the busiest.
        let most = view.workers.iter().map(|w| w.regions).max().unwrap_or(0);
        let least = view.workers.iter().map(|w| w.regions).min().unwrap_or(0);
        let mut split = Line::new();
        split.push(MUTED, "  regions per thread ");
        sparkline(
            &mut split,
            view.workers.iter().map(|w| w.regions as f64),
            width.saturating_sub(48).min(view.workers.len()),
            |_| palette::ACCENT,
        );
        split.push(MUTED, format!("  {} to {}", group(least), group(most)));
        lines.push(split);
        return lines;
    }

    let image_name = |worker: &WorkerView| {
        worker
            .current
            .image
            .and_then(|i| view.images.get(i))
            .map_or(String::new(), |i| i.name.clone())
    };
    let name_width = view
        .workers
        .iter()
        .map(|w| text_width(&image_name(w)))
        .max()
        .unwrap_or(0)
        .clamp(6, 20);
    let timeline = width.saturating_sub(name_width + 70).clamp(8, 32);

    let row = |worker: &WorkerView, history: Option<&WorkerHistory>| {
        let mut line = Line::new();
        line.push(MUTED, format!("  {:>2} ", worker.index));
        let samples: Vec<Sample> =
            history.map_or(Vec::new(), |h| h.samples.iter().copied().collect());
        let padding = timeline.saturating_sub(samples.len());
        line.push(FAINT, "▕").push(PLAIN, " ".repeat(padding));
        for sample in samples.iter().skip(samples.len().saturating_sub(timeline)) {
            let level = (sample.busy * 8.0).round().clamp(0.0, 8.0) as usize;
            line.push_char(fg(activity_color(sample.doing)), LEVELS[level]);
        }
        line.push(FAINT, "▏ ");
        let current = &worker.current;
        if current.image.is_none() || worker.activity == Activity::Idle {
            line.push(FAINT, format!("{:<10}", activity_label(Activity::Idle)))
                .push(MUTED, format!("{:>8} done", group(worker.regions)));
            return line;
        }
        let elapsed = view.elapsed.saturating_sub(current.since);
        let elapsed_style = match elapsed {
            e if e >= STUCK => bold(palette::BAD),
            e if e >= SLOW => fg(palette::WARN),
            _ => MUTED,
        };
        line.push(
            fg(activity_color(worker.activity)),
            format!("{:<10}", activity_label(worker.activity)),
        )
        .push(elapsed_style, format!("{:>6}", duration(elapsed)))
        .push(
            MUTED,
            format!(
                " {:>6}",
                match current.instructions {
                    0 => "…".into(),
                    n => short(n),
                }
            ),
        )
        .push(PLAIN, " ");
        match current.hot {
            true => line.push(fg(palette::HOT), "■ "),
            false => line.push(fg(palette::COLD), "■ "),
        };
        line.push(PLAIN, truncate(&image_name(worker), name_width));
        line.pad(line.width() + name_width.saturating_sub(text_width(&image_name(worker))) + 1);
        line.push(MUTED, format!("{:#x} ", current.address));
        match (current.name.is_empty(), current.source) {
            (false, _) => line.push(PLAIN, current.name.clone()),
            (true, Some(source)) => line.push(FAINT, format!("({})", source_label(source))),
            (true, None) => line.push(PLAIN, ""),
        };
        line
    };

    if !compact {
        for (n, worker) in view.workers.iter().enumerate() {
            lines.push(row(worker, tracker.workers.get(n)));
        }
        return lines;
    }

    // One cell a thread, its height how busy it was lately.
    let per_line = (width.saturating_sub(4) / 2).max(1);
    for chunk in view.workers.chunks(per_line) {
        let mut line = Line::new();
        line.push(PLAIN, "  ");
        for worker in chunk {
            let recent = tracker
                .workers
                .get(worker.index)
                .and_then(|h| h.samples.back().copied());
            let level = recent.map_or(0, |s| (s.busy * 8.0).round().clamp(0.0, 8.0) as usize);
            let glyph = match worker.activity {
                Activity::Idle => '·',
                _ => LEVELS[level.max(1)],
            };
            line.push_char(fg(activity_color(worker.activity)), glyph)
                .push(PLAIN, " ");
        }
        lines.push(line);
    }
    let mut slowest: Vec<&WorkerView> = view
        .workers
        .iter()
        .filter(|w| w.current.image.is_some() && w.activity != Activity::Idle)
        .collect();
    slowest.sort_by_key(|w| w.current.since);
    let mut label = Line::new();
    label.push(MUTED, "  longest in flight");
    lines.push(label);
    for worker in slowest.into_iter().take(3) {
        lines.push(row(worker, tracker.workers.get(worker.index)));
    }
    lines
}

fn events(view: &BoardView, width: usize, keep: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    for event in view.events.iter().rev().take(keep).rev() {
        let mut line = Line::new();
        let (glyph, color) = match event.kind {
            EventKind::Info => ('·', palette::MUTED),
            EventKind::Good => ('✓', palette::GOOD),
            EventKind::Warn => ('!', palette::WARN),
            EventKind::Bad => ('✗', palette::BAD),
        };
        line.push(FAINT, format!("  {:>7} ", clock(event.at)))
            .push_char(bold(color), glyph)
            .push(PLAIN, " ")
            .push(fg(color), truncate(&event.text, width.saturating_sub(14)));
        lines.push(line);
    }
    lines
}

/// One filled stretch of a bar, in cells.
struct Fill {
    from: f64,
    to: f64,
    color: u8,
}

/// Draws `width` cells, `fills` over a background coloured by `empty`.
fn bar(line: &mut Line, width: usize, fills: &[Fill], empty: impl Fn(usize) -> u8) {
    line.push(FAINT, "▕");
    for cell in 0..width {
        let at = cell as f64;
        let overlap = |fill: &Fill| (fill.to.min(at + 1.0) - fill.from.max(at)).max(0.0);
        let covered: f64 = fills.iter().map(overlap).sum();
        // A cell shared by two fills takes the colour of the larger; a partial one
        // is drawn only when it fills from the left.
        let largest = fills
            .iter()
            .max_by(|a, b| overlap(a).total_cmp(&overlap(b)))
            .filter(|fill| overlap(fill) > 0.0);
        let from_left = fills.iter().any(|f| overlap(f) > 0.0 && f.from <= at);
        match largest {
            Some(fill) if covered >= 0.9375 => line.push_char(fg(fill.color), '█'),
            // At least a quarter: an eighth is too easily taken for the bar's end.
            Some(fill) if covered >= 0.1875 && from_left => {
                let eighth = ((covered * 8.0).round() as usize).clamp(2, 7);
                line.push_char(fg(fill.color), EIGHTHS[eighth - 1])
            }
            _ => line.push_char(fg(empty(cell)), '░'),
        };
    }
    line.push(FAINT, "▏");
}

/// Draws `width` cells split among `parts` in proportion.
fn stacked(line: &mut Line, width: usize, parts: &[(Duration, u8)]) {
    let total: f64 = parts.iter().map(|(d, _)| d.as_secs_f64()).sum();
    line.push(FAINT, "▕");
    if total <= 0.0 {
        line.push(FAINT, "░".repeat(width));
    } else {
        // Largest remainders, so the cells add up to `width` exactly.
        let exact: Vec<f64> = parts
            .iter()
            .map(|(d, _)| d.as_secs_f64() / total * width as f64)
            .collect();
        let mut cells: Vec<usize> = exact.iter().map(|e| e.floor() as usize).collect();
        let mut short_by = width - cells.iter().sum::<usize>();
        let mut order: Vec<usize> = (0..parts.len()).collect();
        order.sort_by(|&a, &b| {
            (exact[b] - exact[b].floor()).total_cmp(&(exact[a] - exact[a].floor()))
        });
        for n in order {
            if short_by == 0 {
                break;
            }
            cells[n] += 1;
            short_by -= 1;
        }
        for (&(_, color), count) in parts.iter().zip(cells) {
            let glyph = if color == palette::FAINT {
                "░"
            } else {
                "█"
            };
            line.push(fg(color), glyph.repeat(count));
        }
    }
    line.push(FAINT, "▏");
}

/// The last `width` of `values` as bars against the largest of them.
fn sparkline(
    line: &mut Line,
    values: impl Iterator<Item = f64>,
    width: usize,
    color: impl Fn(usize) -> u8,
) {
    let values: Vec<f64> = values.collect();
    let shown = &values[values.len().saturating_sub(width)..];
    let most = shown.iter().copied().fold(0.0, f64::max);
    line.push(PLAIN, " ".repeat(width - shown.len()));
    for (n, &value) in shown.iter().enumerate() {
        let level = match most > 0.0 {
            true => ((value / most * 8.0).round() as usize).clamp(1, 8),
            false => 1,
        };
        line.push_char(fg(color(n)), LEVELS[level]);
    }
}

fn spinner(tick: u64) -> char {
    SPINNER[(tick % SPINNER.len() as u64) as usize]
}

/// Images in the order they are listed: work in progress first.
fn image_rank(state: ImageState) -> u8 {
    match state {
        ImageState::Compiling => 0,
        ImageState::Reading | ImageState::Discovering | ImageState::Planning => 1,
        ImageState::Queued => 2,
        ImageState::Failed => 3,
        ImageState::Stopped => 4,
        ImageState::Done => 5,
        ImageState::Skipped => 6,
    }
}

fn state_color(state: ImageState) -> u8 {
    match state {
        ImageState::Compiling => palette::ACCENT,
        ImageState::Done => palette::GOOD,
        ImageState::Stopped => palette::WARN,
        ImageState::Failed => palette::BAD,
        _ => palette::MUTED,
    }
}

fn activity_color(activity: Activity) -> u8 {
    match activity {
        Activity::Idle => palette::FAINT,
        Activity::Lifting => palette::LIFT,
        Activity::Waiting => palette::WAIT,
        Activity::Compiling => palette::COMPILE,
        Activity::Writing => palette::WRITE,
        Activity::Committing => palette::COMMIT,
    }
}

fn activity_label(activity: Activity) -> &'static str {
    match activity {
        Activity::Idle => "idle",
        Activity::Lifting => "lift",
        Activity::Waiting => "wait",
        Activity::Compiling => "compile",
        Activity::Writing => "write",
        Activity::Committing => "commit",
    }
}

fn source_color(source: Source) -> u8 {
    match source {
        Source::Profiled
        | Source::Load
        | Source::Method
        | Source::Initializer
        | Source::EntryPoint => palette::HOT,
        _ => palette::COLD,
    }
}

fn source_label(source: Source) -> &'static str {
    match source {
        Source::Profiled => "profiled",
        Source::Load => "+load",
        Source::Method => "objc methods",
        Source::Initializer => "initializers",
        Source::EntryPoint => "entry point",
        Source::Unwind => "with exception tables",
        Source::LandingPad => "landing pads",
        Source::FunctionStart => "function starts",
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// `text`, cut to `width` columns with an ellipsis.
fn truncate(text: &str, width: usize) -> String {
    if text_width(text) <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_width(c);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// `1234567` as `1,234,567`.
fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// `1234567` as `1.2M`.
fn short(n: usize) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}K", n as f64 / 1e3),
        10_000..1_000_000 => format!("{}K", n / 1_000),
        1_000_000..10_000_000 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{}M", n / 1_000_000),
    }
}

fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `75s` as `1:15`.
fn clock(d: Duration) -> String {
    let s = d.as_secs();
    match s >= 3600 {
        true => format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60),
        false => format!("{}:{:02}", s / 60, s % 60),
    }
}

/// A short span: `340ms`, `2.4s`, `1:15`.
fn duration(d: Duration) -> String {
    match d.as_millis() {
        0..1_000 => format!("{}ms", d.as_millis()),
        1_000..60_000 => format!("{:.1}s", d.as_secs_f64()),
        _ => clock(d),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: Size = Size {
        width: 160,
        height: 60,
    };

    fn text(frame: &[Line]) -> String {
        frame.iter().map(|l| l.text() + "\n").collect()
    }

    fn compiling() -> Board {
        let board = Board::new("/Applications/Game.app/Contents/MacOS/Game", true);
        let game = board.image("/Applications/Game.app/Contents/MacOS/Game");
        game.discovered(10, &[2, 0, 3, 1, 1, 0, 0, 3]);
        game.planned(8, 4);
        let cached = board.image("/Applications/Game.app/Contents/Frameworks/Old.dylib");
        cached.skipped(&board, "already cached");
        board.set_phase(Phase::Compiling, "");
        let workers = board.workers(2);
        workers[0].set(Activity::Compiling);
        workers[0].begin(
            0,
            0x1000_0040,
            Source::Method,
            Some(("-[Player jump]", 0)),
            true,
        );
        game.started();
        game.region_finished(true, true);
        game.region_finished(true, false);
        board
    }

    #[test]
    fn groups_and_shortens_counts() {
        assert_eq!(group(0), "0");
        assert_eq!(group(1_234_567), "1,234,567");
        assert_eq!(short(999), "999");
        assert_eq!(short(1_234), "1.2K");
        assert_eq!(short(412_330), "412K");
        assert_eq!(bytes(3 << 30), "3.0 GiB");
        assert_eq!(clock(Duration::from_secs(3725)), "1:02:05");
    }

    #[test]
    fn a_line_is_cut_to_the_terminal_width() {
        let mut line = Line::new();
        line.push(PLAIN, "abcdef").push(MUTED, "ghij");
        let mut out = String::new();
        assert_eq!(line.write(&mut out, 6, false), 6);
        assert_eq!(out, "abcde…");
        let mut out = String::new();
        assert_eq!(line.write(&mut out, 10, false), 10);
        assert_eq!(out, "abcdefghij");
    }

    #[test]
    fn wide_characters_take_two_columns() {
        assert_eq!(text_width("東京"), 4);
        assert_eq!(truncate("東京ゲーム", 5), "東京…");
    }

    #[test]
    fn a_bar_fills_partial_cells_from_the_left() {
        let mut line = Line::new();
        let fill = Fill {
            from: 0.0,
            to: 2.5,
            color: palette::HOT,
        };
        bar(&mut line, 4, &[fill], |_| palette::FAINT);
        assert_eq!(line.text(), "▕██▌░▏");
    }

    #[test]
    fn a_finished_bar_is_full_however_its_fills_split_a_cell() {
        let mut line = Line::new();
        let fills = [
            Fill {
                from: 0.0,
                to: 0.01,
                color: palette::HOT,
            },
            Fill {
                from: 0.01,
                to: 4.0,
                color: palette::COLD,
            },
        ];
        bar(&mut line, 4, &fills, |_| palette::FAINT);
        assert_eq!(line.text(), "▕████▏");
    }

    #[test]
    fn stacked_cells_add_up_to_the_width() {
        let mut line = Line::new();
        let third = Duration::from_secs(1);
        stacked(&mut line, 10, &[(third, 1), (third, 2), (third, 3)]);
        assert_eq!(text_width(&line.text()), 12);
    }

    #[test]
    fn shows_what_each_thread_compiles() {
        let board = compiling();
        let view = board.view();
        let frame = text(&render(&view, &Tracker::default(), WIDE, 0, false));
        assert!(frame.contains("rosemary aot · Game"), "{frame}");
        assert!(frame.contains("2 / 8 regions"), "{frame}");
        assert!(frame.contains("-[Player jump]"), "{frame}");
        assert!(frame.contains("0x10000040"), "{frame}");
        assert!(frame.contains("already cached ×1"), "{frame}");
    }

    #[test]
    fn a_short_terminal_gets_a_compact_core_map() {
        let board = compiling();
        board.workers(64);
        let view = board.view();
        let size = Size {
            width: 120,
            height: 20,
        };
        let frame = render(&view, &Tracker::default(), size, 0, false);
        assert!(frame.len() <= size.height, "{}", text(&frame));
        assert!(text(&frame).contains("longest in flight"));
    }

    #[test]
    fn planning_shows_where_entries_came_from() {
        let board = Board::new("Game", true);
        let image = board.image("/Game");
        board.set_phase(Phase::Discovering, "");
        image.discovered(9, &[2, 1, 3, 0, 1, 0, 0, 2]);
        image.examining(4, 0x1000, 3);
        let frame = text(&render(&board.view(), &Tracker::default(), WIDE, 0, false));
        assert!(frame.contains("3 objc methods"), "{frame}");
        assert!(frame.contains("4 / 9 entries → 3 regions"), "{frame}");
    }

    #[test]
    fn the_last_frame_shows_how_regions_were_split() {
        let board = compiling();
        board.set_phase(Phase::Done, "");
        let frame = text(&render(&board.view(), &Tracker::default(), WIDE, 0, true));
        assert!(frame.contains("regions per thread"), "{frame}");
    }
}
