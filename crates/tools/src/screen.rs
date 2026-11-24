//! The terminal screen: a command's bytes worked into lines, as its
//! terminal would show them.

use std::collections::VecDeque;

use caf_core::tool::MOST_LINE;
use vte::{Params, Parser, Perform};

/// The screen's height, in rows: how far up a command can move its
/// cursor to rewrite what it printed. The pty a command runs in is given
/// this size.
pub const ROWS: usize = 24;
/// The screen's width, in columns, that a command is told. Lines aren't
/// wrapped at it: a longer line stays one line.
pub const COLUMNS: usize = 200;
/// The most characters a line holds: past this, it goes on on the next
/// line, as if it wrapped.
const MOST_CELLS: usize = MOST_LINE;

/// A change to the lines a [`Screen`] has given: the lines from `from`
/// on are now `lines`, and there are none after them. A line before
/// `from` is as it was.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redraw {
    /// The first line replaced, numbered from 0.
    pub from: u64,
    /// The lines from there.
    pub lines: Vec<String>,
}

/// A virtual terminal screen that works a command's output into lines.
///
/// - **Lines carry their colours** as SGR codes, each line on its own: it
///   starts from the default style and, if it doesn't end in it, resets.
/// - **The cursor's movements are worked out,** within the last [`ROWS`]
///   lines: a carriage return, backspace, erasing in the line or the
///   screen, and moving the cursor up, down or to a column, so a progress
///   bar is its last drawing, not each one. Lines above those are as
///   they were when they scrolled away.
/// - **A line feed starts a new line at its start,** as a pty's output
///   does, its `\n` made `\r\n`.
/// - **Lines aren't wrapped,** at [`COLUMNS`] or anywhere short of
///   16 KiB of characters.
/// - **Anything else is dropped:** titles and other OSC strings, DCS,
///   modes, mouse reports.
///
/// Its memory is bounded by its window's rows and their length, and by
/// what one [`feed`](Self::feed) gives back.
pub struct Screen {
    parser: Parser,
    grid: Grid,
    /// The start of a character the last feed ended in the middle of.
    carry: Vec<u8>,
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Screen {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Screen")
            .field("top", &self.grid.top)
            .field("row", &self.grid.row)
            .field("col", &self.grid.col)
            .finish_non_exhaustive()
    }
}

impl Screen {
    /// An empty screen.
    #[must_use]
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            grid: Grid::new(),
            carry: Vec::new(),
        }
    }

    /// Takes the next of a command's output, and gives back how its lines
    /// changed, if they did: one change, for all of it.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<Redraw> {
        // The parser is never given part of a character: vte 0.15.0 drops
        // the character after one it's given in two parts, if the bytes
        // after that end in the middle of another.
        let joined;
        let bytes = if self.carry.is_empty() {
            bytes
        } else {
            joined = [std::mem::take(&mut self.carry).as_slice(), bytes].concat();
            &joined
        };
        let whole = bytes.len() - split_char(bytes);
        self.parser.advance(&mut self.grid, &bytes[..whole]);
        self.carry.extend_from_slice(&bytes[whole..]);
        self.grid.redraw()
    }
}

/// How many bytes at the end of `bytes` are the start of a UTF-8
/// character that isn't finished.
fn split_char(bytes: &[u8]) -> usize {
    for back in 1..=bytes.len().min(3) {
        let byte = bytes[bytes.len() - back];
        if byte & 0xC0 == 0x80 {
            continue;
        }
        let needs = match byte {
            0xF0.. => 4,
            0xE0.. => 3,
            0xC0.. => 2,
            _ => 1,
        };
        return if needs > back { back } else { 0 };
    }
    0
}

/// `line` without its SGR codes, or any other control sequence: what the
/// model reads of a line a [`Screen`] gave.
#[must_use]
pub fn strip_sgr(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            // Parameters and intermediates, up to the final byte.
            for c in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// A colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// How a character is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Style {
    fg: Color,
    bg: Color,
    flags: u8,
}

const BOLD: u8 = 1;
const DIM: u8 = 1 << 1;
const ITALIC: u8 = 1 << 2;
const UNDERLINE: u8 = 1 << 3;
const BLINK: u8 = 1 << 4;
const INVERSE: u8 = 1 << 5;
const HIDDEN: u8 = 1 << 6;
const STRIKE: u8 = 1 << 7;

/// Each flag, and its SGR code.
const FLAGS: [(u8, u8); 8] = [
    (BOLD, 1),
    (DIM, 2),
    (ITALIC, 3),
    (UNDERLINE, 4),
    (BLINK, 5),
    (INVERSE, 7),
    (HIDDEN, 8),
    (STRIKE, 9),
];

impl Style {
    /// The SGR code that sets it, from the default.
    fn sgr(self, out: &mut String) {
        use std::fmt::Write as _;
        out.push_str("\x1b[0");
        for (flag, code) in FLAGS {
            if self.flags & flag != 0 {
                let _ = write!(out, ";{code}");
            }
        }
        let color = |out: &mut String, color: Color, base: u8, bright: u8, extended: u8| {
            let _ = match color {
                Color::Default => Ok(()),
                Color::Indexed(n @ 0..8) => write!(out, ";{}", base + n),
                Color::Indexed(n @ 8..16) => write!(out, ";{}", bright + n - 8),
                Color::Indexed(n) => write!(out, ";{extended};5;{n}"),
                Color::Rgb(r, g, b) => write!(out, ";{extended};2;{r};{g};{b}"),
            };
        };
        color(out, self.fg, 30, 90, 38);
        color(out, self.bg, 40, 100, 48);
        out.push('m');
    }

    /// Changed by an SGR sequence's parameters.
    fn apply(&mut self, params: &Params) {
        let mut params = params.iter().peekable();
        if params.peek().is_none() {
            *self = Self::default();
            return;
        }
        while let Some(param) = params.next() {
            let code = param[0];
            match code {
                0 => *self = Self::default(),
                1..=9 => {
                    if let Some(&(flag, _)) = FLAGS.iter().find(|(_, c)| u16::from(*c) == code) {
                        self.flags |= flag;
                    }
                }
                21 | 22 => self.flags &= !(BOLD | DIM),
                23 => self.flags &= !ITALIC,
                24 => self.flags &= !UNDERLINE,
                25 => self.flags &= !BLINK,
                27 => self.flags &= !INVERSE,
                28 => self.flags &= !HIDDEN,
                29 => self.flags &= !STRIKE,
                30..=37 => self.fg = Color::Indexed((code - 30) as u8),
                39 => self.fg = Color::Default,
                40..=47 => self.bg = Color::Indexed((code - 40) as u8),
                49 => self.bg = Color::Default,
                90..=97 => self.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.bg = Color::Indexed((code - 100 + 8) as u8),
                38 | 48 => {
                    // `38;5;n`, `38;2;r;g;b`, or the same with colons.
                    let rest: Vec<u16> = if param.len() > 1 {
                        param[1..].to_vec()
                    } else {
                        let mut rest = Vec::new();
                        if let Some(kind) = params.next() {
                            rest.push(kind[0]);
                            let more = if kind[0] == 2 { 3 } else { 1 };
                            for _ in 0..more {
                                if let Some(next) = params.next() {
                                    rest.push(next[0]);
                                }
                            }
                        }
                        rest
                    };
                    let byte = |n: u16| u8::try_from(n).unwrap_or(u8::MAX);
                    let color = match rest.as_slice() {
                        [5, n, ..] => Some(Color::Indexed(byte(*n))),
                        // With a colour space id, as `38:2::r:g:b` has.
                        [2, _, r, g, b] => Some(Color::Rgb(byte(*r), byte(*g), byte(*b))),
                        [2, r, g, b, ..] => Some(Color::Rgb(byte(*r), byte(*g), byte(*b))),
                        _ => None,
                    };
                    if let Some(color) = color {
                        if code == 38 {
                            self.fg = color;
                        } else {
                            self.bg = color;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

/// One character on the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    c: char,
    style: Style,
}

const BLANK: Cell = Cell {
    c: ' ',
    style: Style {
        fg: Color::Default,
        bg: Color::Default,
        flags: 0,
    },
};

/// The screen's state, which the parser changes.
struct Grid {
    /// The window's rows, the first of them line `top`. Rows past the
    /// last are empty.
    rows: VecDeque<Vec<Cell>>,
    /// The number of the window's first line.
    top: u64,
    /// The cursor's row, in the window, and column.
    row: usize,
    col: usize,
    style: Style,
    saved: (usize, usize, Style),
    /// How many lines have been given.
    given: u64,
    /// The first line changed since lines were last given: `u64::MAX` if
    /// none.
    dirty: u64,
    /// Lines that scrolled away since lines were last given, from
    /// `scrolled_from`, that are to be given.
    scrolled: Vec<String>,
    scrolled_from: u64,
}

impl Grid {
    fn new() -> Self {
        Self {
            rows: VecDeque::with_capacity(ROWS),
            top: 0,
            row: 0,
            col: 0,
            style: Style::default(),
            saved: (0, 0, Style::default()),
            given: 0,
            dirty: u64::MAX,
            scrolled: Vec::new(),
            scrolled_from: 0,
        }
    }

    /// Row `row` of the window changed.
    fn touch(&mut self, row: usize) {
        self.dirty = self.dirty.min(self.top + row as u64);
    }

    /// The cursor's row, made if it isn't yet.
    fn current(&mut self) -> &mut Vec<Cell> {
        while self.rows.len() <= self.row {
            self.rows.push_back(Vec::new());
        }
        &mut self.rows[self.row]
    }

    fn put(&mut self, c: char) {
        if self.col >= MOST_CELLS {
            self.line_feed();
        }
        let (col, style) = (self.col, self.style);
        let line = self.current();
        if line.len() < col {
            line.resize(col, BLANK);
        }
        let cell = Cell { c, style };
        if col == line.len() {
            line.push(cell);
        } else {
            line[col] = cell;
        }
        self.col += 1;
        self.touch(self.row);
    }

    /// Down a line, to its start, scrolling the window if it's at its
    /// bottom.
    fn line_feed(&mut self) {
        self.index();
        self.col = 0;
    }

    /// Down a line, scrolling the window if it's at its bottom.
    fn index(&mut self) {
        if self.row + 1 < ROWS {
            self.row += 1;
            return;
        }
        self.current();
        let line = self.rows.pop_front().unwrap_or_default();
        let number = self.top;
        self.top += 1;
        if number >= self.dirty.min(self.given) {
            if self.scrolled.is_empty() {
                self.scrolled_from = number;
            }
            self.scrolled.push(render(&line));
        }
    }

    fn erase_in_line(&mut self, mode: u16) {
        let col = self.col;
        let Some(line) = self.rows.get_mut(self.row) else {
            return;
        };
        match mode {
            0 => line.truncate(col),
            1 => {
                let end = (col + 1).min(line.len());
                line[..end].fill(BLANK);
            }
            2 => line.clear(),
            _ => return,
        }
        self.touch(self.row);
    }

    fn erase_in_display(&mut self, mode: u16) {
        match mode {
            0 => {
                self.erase_in_line(0);
                self.rows.truncate(self.row + 1);
                self.touch(self.row);
            }
            1 => {
                for row in 0..self.row.min(self.rows.len()) {
                    self.rows[row].clear();
                }
                self.erase_in_line(1);
                self.touch(0);
            }
            2 | 3 => {
                self.rows.clear();
                self.touch(0);
            }
            _ => {}
        }
    }

    fn move_to_col(&mut self, col: usize) {
        self.col = col.min(MOST_CELLS - 1);
    }

    fn move_to_row(&mut self, row: usize) {
        self.row = row.min(ROWS - 1);
    }

    /// What's changed since lines were last given, if anything.
    fn redraw(&mut self) -> Option<Redraw> {
        let content = self.rows.iter().rposition(|row| !row.is_empty());
        let end = self.top + content.map_or(0, |at| at as u64 + 1);
        let from = self.dirty.min(self.given).min(end);
        let scrolled = std::mem::take(&mut self.scrolled);
        self.dirty = u64::MAX;
        if from == end && end == self.given {
            return None;
        }
        let mut lines = Vec::new();
        if from < self.top {
            debug_assert_eq!(from, self.scrolled_from, "every line scrolled away is kept");
            let skip = usize::try_from(from.saturating_sub(self.scrolled_from)).unwrap_or(0);
            lines.extend(scrolled.into_iter().skip(skip));
        }
        let start = usize::try_from(from.saturating_sub(self.top)).unwrap_or(usize::MAX);
        let stop = usize::try_from(end - self.top).unwrap_or(0);
        lines.extend((start..stop).map(|row| render(&self.rows[row])));
        self.given = end;
        Some(Redraw { from, lines })
    }
}

/// `line` as text, with its colours as SGR codes from the default style.
fn render(line: &[Cell]) -> String {
    let mut out = String::with_capacity(line.len());
    let mut style = Style::default();
    for cell in line {
        if cell.style != style {
            style = cell.style;
            if style == Style::default() {
                out.push_str("\x1b[0m");
            } else {
                style.sgr(&mut out);
            }
        }
        out.push(cell.c);
    }
    if style != Style::default() {
        out.push_str("\x1b[0m");
    }
    out
}

/// A parameter, or `default` if it's missing or 0, as most cursor
/// movements take theirs.
fn count(params: &Params, at: usize, default: u16) -> usize {
    match params.iter().nth(at).map(|param| param[0]) {
        None | Some(0) => usize::from(default),
        Some(n) => usize::from(n),
    }
}

impl Perform for Grid {
    fn print(&mut self, c: char) {
        self.put(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => self.line_feed(),
            b'\r' => self.col = 0,
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => self.move_to_col((self.col / 8 + 1) * 8),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        // Private modes (`?25l` and the like) and anything malformed.
        if ignore || !intermediates.is_empty() {
            return;
        }
        let n = count(params, 0, 1);
        match action {
            'm' => self.style.apply(params),
            'K' => self.erase_in_line(params.iter().next().map_or(0, |p| p[0])),
            'J' => self.erase_in_display(params.iter().next().map_or(0, |p| p[0])),
            'A' => self.row = self.row.saturating_sub(n),
            'B' | 'e' => self.move_to_row(self.row.saturating_add(n)),
            'C' | 'a' => self.move_to_col(self.col.saturating_add(n)),
            'D' => self.col = self.col.saturating_sub(n),
            'E' => {
                self.move_to_row(self.row.saturating_add(n));
                self.col = 0;
            }
            'F' => {
                self.row = self.row.saturating_sub(n);
                self.col = 0;
            }
            'G' | '`' => self.move_to_col(n - 1),
            'd' => self.move_to_row(n - 1),
            'H' | 'f' => {
                self.move_to_row(n - 1);
                self.move_to_col(count(params, 1, 1) - 1);
            }
            'X' => {
                let col = self.col;
                if let Some(line) = self.rows.get_mut(self.row) {
                    let end = col.saturating_add(n).min(line.len());
                    if col < end {
                        line[col..end].fill(BLANK);
                    }
                    self.touch(self.row);
                }
            }
            'P' => {
                let col = self.col;
                if let Some(line) = self.rows.get_mut(self.row) {
                    let end = col.saturating_add(n).min(line.len());
                    if col < end {
                        line.drain(col..end);
                    }
                    self.touch(self.row);
                }
            }
            '@' => {
                let col = self.col;
                if let Some(line) = self.rows.get_mut(self.row)
                    && col < line.len()
                {
                    let n = n.min(MOST_CELLS - col);
                    line.splice(col..col, std::iter::repeat_n(BLANK, n));
                    line.truncate(MOST_CELLS);
                    self.touch(self.row);
                }
            }
            'L' => {
                if self.row < self.rows.len() {
                    for _ in 0..n.min(ROWS) {
                        self.rows.insert(self.row, Vec::new());
                    }
                    self.rows.truncate(ROWS);
                    self.touch(self.row);
                }
            }
            'M' => {
                if self.row < self.rows.len() {
                    let end = self.row.saturating_add(n).min(self.rows.len());
                    self.rows.drain(self.row..end);
                    self.touch(self.row);
                }
            }
            's' => self.saved = (self.row, self.col, self.style),
            'u' => (self.row, self.col, self.style) = self.saved,
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], _ignore: bool, byte: u8) {
        if !intermediates.is_empty() {
            return;
        }
        match byte {
            b'7' => self.saved = (self.row, self.col, self.style),
            b'8' => (self.row, self.col, self.style) = self.saved,
            b'D' => self.index(),
            b'E' => self.line_feed(),
            b'M' => self.row = self.row.saturating_sub(1),
            b'c' => {
                self.rows.clear();
                self.touch(0);
                (self.row, self.col, self.style) = (0, 0, Style::default());
                self.saved = (0, 0, Style::default());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;
