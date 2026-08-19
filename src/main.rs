use std::env;
use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::time::Duration;

use crossterm::{
    cursor::{Hide, MoveTo, SetCursorStyle, Show},
    event::{
        self, DisableFocusChange, EnableFocusChange, Event, KeyCode, KeyEvent,
        KeyModifiers,
    },
    execute, queue,
    style::Print,
    terminal::{
        self, BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Normal,
    Insert,
}

/// Pending multi-key command in normal mode (`d` of `dd`, `g` of `gg`, `Z` of `ZZ`).
#[derive(Clone, Copy, PartialEq)]
enum Pending {
    None,
    D,
    G,
    Z,
}

struct Editor {
    lines: Vec<String>,
    row: usize,
    /// Desired column in chars; clamped per line when rendering/moving.
    col: usize,
    mode: Mode,
    pending: Pending,
    path: Option<PathBuf>,
    /// Center the whole block (aligned on the widest line) instead of
    /// centering each line individually.
    block: bool,
    dirty: bool,
    undo: Vec<(Vec<String>, usize, usize)>,
    top: usize,
    quit: bool,
    /// Save-as popup input; `Some` while the popup is open.
    prompt: Option<String>,
    /// Normal-mode cursor visibility: hidden by default, shown for a short
    /// while after a keypress.
    show_cursor: bool,
}

impl Editor {
    fn new(path: Option<PathBuf>, block: bool) -> io::Result<Self> {
        let lines = match &path {
            Some(p) if p.exists() => {
                let text = fs::read_to_string(p)?;
                let mut lines: Vec<String> =
                    text.lines().map(str::to_string).collect();
                if lines.is_empty() {
                    lines.push(String::new());
                }
                lines
            }
            _ => vec![String::new()],
        };
        // With no file to open there is nothing to navigate yet; drop
        // straight into insert mode.
        let mode = if path.is_none() { Mode::Insert } else { Mode::Normal };
        Ok(Self {
            lines,
            row: 0,
            col: 0,
            mode,
            pending: Pending::None,
            path,
            block,
            dirty: false,
            undo: Vec::new(),
            top: 0,
            quit: false,
            prompt: None,
            show_cursor: false,
        })
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    /// Max cursor column on the current line for the current mode
    /// (normal mode stops on the last char, insert mode goes one past).
    fn max_col(&self) -> usize {
        let len = self.line_len(self.row);
        match self.mode {
            Mode::Insert => len,
            Mode::Normal => len.saturating_sub(1),
        }
    }

    fn clamped_col(&self) -> usize {
        self.col.min(self.max_col())
    }

    fn byte_idx(&self, row: usize, col: usize) -> usize {
        self.lines[row]
            .char_indices()
            .nth(col)
            .map(|(i, _)| i)
            .unwrap_or(self.lines[row].len())
    }

    fn snapshot(&mut self) {
        self.undo.push((self.lines.clone(), self.row, self.clamped_col()));
        if self.undo.len() > 1000 {
            self.undo.remove(0);
        }
        self.dirty = true;
    }

    fn restore(&mut self) {
        if let Some((lines, row, col)) = self.undo.pop() {
            self.lines = lines;
            self.row = row;
            self.col = col;
            self.dirty = true;
        }
    }

    fn save(&self) -> io::Result<()> {
        if let Some(p) = &self.path {
            let mut text = self.lines.join("\n");
            text.push('\n');
            fs::write(p, text)?;
        }
        Ok(())
    }

    /// Write the buffer back to its file whenever it has unsaved edits.
    fn autosave(&mut self) -> io::Result<()> {
        if self.dirty && self.path.is_some() {
            self.save()?;
            self.dirty = false;
        }
        Ok(())
    }

    // --- editing primitives -------------------------------------------------

    fn insert_char(&mut self, c: char) {
        let col = self.clamped_col();
        let idx = self.byte_idx(self.row, col);
        self.lines[self.row].insert(idx, c);
        self.col = col + 1;
    }

    fn insert_newline(&mut self) {
        let col = self.clamped_col();
        let idx = self.byte_idx(self.row, col);
        let rest = self.lines[self.row].split_off(idx);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    fn backspace(&mut self) {
        let col = self.clamped_col();
        if col > 0 {
            let idx = self.byte_idx(self.row, col - 1);
            self.lines[self.row].remove(idx);
            self.col = col - 1;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len(self.row);
            self.lines[self.row].push_str(&cur);
        }
    }

    fn delete_char(&mut self) {
        let col = self.clamped_col();
        if col < self.line_len(self.row) {
            self.snapshot();
            let idx = self.byte_idx(self.row, col);
            self.lines[self.row].remove(idx);
            self.col = col;
        }
    }

    fn delete_line(&mut self) {
        self.snapshot();
        if self.lines.len() == 1 {
            self.lines[0].clear();
        } else {
            self.lines.remove(self.row);
            if self.row >= self.lines.len() {
                self.row = self.lines.len() - 1;
            }
        }
        self.col = self.clamped_col();
    }

    // --- motions ------------------------------------------------------------

    fn move_h(&mut self, dx: isize) {
        let col = self.clamped_col() as isize + dx;
        self.col = col.clamp(0, self.max_col() as isize) as usize;
    }

    fn move_v(&mut self, dy: isize) {
        let row = self.row as isize + dy;
        self.row = row.clamp(0, self.lines.len() as isize - 1) as usize;
    }

    fn word_forward(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut col = self.clamped_col();
        // Skip current word, then whitespace.
        while col < chars.len() && !chars[col].is_whitespace() {
            col += 1;
        }
        while col < chars.len() && chars[col].is_whitespace() {
            col += 1;
        }
        if col >= chars.len() && self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.lines[self.row]
                .chars()
                .position(|c| !c.is_whitespace())
                .unwrap_or(0);
        } else {
            self.col = col.min(self.max_col());
        }
    }

    fn word_back(&mut self) {
        let mut col = self.clamped_col();
        if col == 0 {
            if self.row > 0 {
                self.row -= 1;
                self.col = self.max_col();
            }
            return;
        }
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        col -= 1;
        while col > 0 && chars[col].is_whitespace() {
            col -= 1;
        }
        while col > 0 && !chars[col - 1].is_whitespace() {
            col -= 1;
        }
        self.col = col;
    }

    fn first_nonblank(&mut self) {
        self.col = self.lines[self.row]
            .chars()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0);
    }

    // --- key handling -------------------------------------------------------

    fn handle_key(&mut self, key: KeyEvent) -> io::Result<()> {
        self.show_cursor = true;
        if self.prompt.is_some() {
            return self.handle_prompt(key);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('c')
        {
            self.quit = true;
            return Ok(());
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('s')
        {
            self.prompt = Some(
                self.path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            );
            return Ok(());
        }
        match self.mode {
            Mode::Insert => self.handle_insert(key),
            Mode::Normal => self.handle_normal(key)?,
        }
        Ok(())
    }

    fn handle_insert(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.col = self.clamped_col();
            }
            KeyCode::Char(c) => self.insert_char(c),
            KeyCode::Enter => self.insert_newline(),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Tab => {
                for _ in 0..4 {
                    self.insert_char(' ');
                }
            }
            KeyCode::Left => self.move_h(-1),
            KeyCode::Right => self.move_h(1),
            KeyCode::Up => self.move_v(-1),
            KeyCode::Down => self.move_v(1),
            _ => {}
        }
    }

    fn handle_normal(&mut self, key: KeyEvent) -> io::Result<()> {
        let pending = self.pending;
        self.pending = Pending::None;

        let code = key.code;
        match pending {
            Pending::D => {
                match code {
                    KeyCode::Char('d') => self.delete_line(),
                    KeyCode::Char('w') => {
                        self.snapshot();
                        let start = self.clamped_col();
                        let sidx = self.byte_idx(self.row, start);
                        let srow = self.row;
                        self.word_forward();
                        if self.row == srow {
                            let eidx = self.byte_idx(self.row, self.clamped_col());
                            self.lines[self.row].replace_range(sidx..eidx, "");
                        } else {
                            self.row = srow;
                            self.lines[srow].truncate(sidx);
                        }
                        self.col = start.min(self.max_col());
                    }
                    _ => {}
                }
                return Ok(());
            }
            Pending::G => {
                if code == KeyCode::Char('g') {
                    self.row = 0;
                    self.col = self.clamped_col();
                }
                return Ok(());
            }
            Pending::Z => {
                match code {
                    KeyCode::Char('Z') => {
                        self.save()?;
                        self.quit = true;
                    }
                    KeyCode::Char('Q') => self.quit = true,
                    _ => {}
                }
                return Ok(());
            }
            Pending::None => {}
        }

        match code {
            KeyCode::Char('h') | KeyCode::Left => self.move_h(-1),
            KeyCode::Char('l') | KeyCode::Right => self.move_h(1),
            KeyCode::Char('j') | KeyCode::Down => self.move_v(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_v(-1),
            KeyCode::Char('0') => self.col = 0,
            KeyCode::Char('$') => self.col = self.max_col(),
            KeyCode::Char('^') => self.first_nonblank(),
            KeyCode::Char('w') => self.word_forward(),
            KeyCode::Char('b') => self.word_back(),
            KeyCode::Char('B') => self.block = !self.block,
            KeyCode::Char('G') => {
                self.row = self.lines.len() - 1;
                self.col = self.clamped_col();
            }
            KeyCode::Char('g') => self.pending = Pending::G,
            KeyCode::Char('d') => self.pending = Pending::D,
            KeyCode::Char('Z') => self.pending = Pending::Z,
            KeyCode::Char('i') => self.enter_insert(0),
            KeyCode::Char('a') => self.enter_insert(1),
            KeyCode::Char('I') => {
                self.first_nonblank();
                self.enter_insert(0);
            }
            KeyCode::Char('A') => {
                self.snapshot();
                self.mode = Mode::Insert;
                self.col = self.max_col();
            }
            KeyCode::Char('o') => {
                self.snapshot();
                self.mode = Mode::Insert;
                self.lines.insert(self.row + 1, String::new());
                self.row += 1;
                self.col = 0;
            }
            KeyCode::Char('O') => {
                self.snapshot();
                self.mode = Mode::Insert;
                self.lines.insert(self.row, String::new());
                self.col = 0;
            }
            KeyCode::Char('x') => self.delete_char(),
            KeyCode::Char('D') => {
                self.snapshot();
                let idx = self.byte_idx(self.row, self.clamped_col());
                self.lines[self.row].truncate(idx);
                self.col = self.clamped_col();
            }
            KeyCode::Char('u') => self.restore(),
            _ => {}
        }
        Ok(())
    }

    fn handle_prompt(&mut self, key: KeyEvent) -> io::Result<()> {
        let input = self.prompt.as_mut().unwrap();
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let name = input.trim().to_string();
                if !name.is_empty() {
                    self.path = Some(PathBuf::from(name));
                    self.save()?;
                    self.dirty = false;
                }
                self.prompt = None;
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Char('c')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.prompt = None;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.push(c);
            }
            _ => {}
        }
        Ok(())
    }

    fn enter_insert(&mut self, offset: usize) {
        self.snapshot();
        let col = self.clamped_col();
        self.mode = Mode::Insert;
        self.col = (col + offset).min(self.line_len(self.row));
    }

    // --- rendering ----------------------------------------------------------

    fn draw(&mut self, out: &mut impl Write) -> io::Result<()> {
        let (w, h) = terminal::size()?;
        let (w, h) = (w as usize, h.max(1) as usize);

        // Vertical window: center the buffer if it fits, otherwise scroll
        // so the cursor stays visible.
        let visible = self.lines.len().min(h);
        let y0 = (h - visible) / 2;
        if self.lines.len() <= h {
            self.top = 0;
        } else {
            if self.row < self.top {
                self.top = self.row;
            }
            if self.row >= self.top + h {
                self.top = self.row - h + 1;
            }
        }

        // Horizontal: either each line is centered on its own width, or the
        // whole block is centered on its widest line (lines left-aligned).
        let block_x0 = self.block.then(|| {
            let max_len = self
                .lines
                .iter()
                .map(|l| l.chars().count())
                .max()
                .unwrap_or(0);
            (w - max_len.min(w)) / 2
        });
        let line_x0 = |line: &str| {
            block_x0.unwrap_or_else(|| (w - line.chars().count().min(w)) / 2)
        };

        // Hide the cursor and batch the whole frame inside a synchronized
        // update so the clear + repaint land on screen atomically.
        queue!(out, BeginSynchronizedUpdate, Hide, Clear(ClearType::All))?;
        for (i, line) in
            self.lines.iter().skip(self.top).take(visible).enumerate()
        {
            let x0 = line_x0(line);
            let clipped: String = line.chars().take(w - x0).collect();
            queue!(
                out,
                MoveTo(x0 as u16, (y0 + i) as u16),
                Print(clipped)
            )?;
        }

        if let Some(input) = &self.prompt {
            self.draw_prompt(out, input.clone(), w, h)?;
        } else {
            let x0 = line_x0(&self.lines[self.row]);
            let cx = (x0 + self.clamped_col()).min(w.saturating_sub(1));
            let cy = y0 + (self.row - self.top);
            let style = match self.mode {
                Mode::Normal => SetCursorStyle::SteadyBlock,
                Mode::Insert => SetCursorStyle::SteadyBar,
            };
            if self.mode == Mode::Insert || self.show_cursor {
                queue!(out, MoveTo(cx as u16, cy as u16), style, Show)?;
            } else {
                // Park the hidden cursor in the top-right corner so a
                // restored session snapshot shows it there, not mid-text.
                queue!(out, MoveTo(w.saturating_sub(1) as u16, 0))?;
            }
        }
        queue!(out, EndSynchronizedUpdate)?;
        out.flush()
    }

    /// Centered "Save as" popup; the cursor sits at the end of the input.
    fn draw_prompt(
        &self,
        out: &mut impl Write,
        input: String,
        w: usize,
        h: usize,
    ) -> io::Result<()> {
        let title = " Save as ";
        let inner = (input.chars().count() + 2)
            .max(title.chars().count())
            .max(30)
            .min(w.saturating_sub(2));
        let bx = (w.saturating_sub(inner + 2)) / 2;
        let by = h.saturating_sub(3) / 2;

        // Show the tail of the input if it is wider than the box.
        let shown: String = input
            .chars()
            .skip(input.chars().count().saturating_sub(inner - 1))
            .collect();

        let top = format!("┌{title}{}┐", "─".repeat(inner - title.chars().count()));
        let mid = format!("│ {shown}{}│", " ".repeat(inner - 1 - shown.chars().count()));
        let bot = format!("└{}┘", "─".repeat(inner));
        queue!(
            out,
            MoveTo(bx as u16, by as u16),
            Print(top),
            MoveTo(bx as u16, (by + 1) as u16),
            Print(mid),
            MoveTo(bx as u16, (by + 2) as u16),
            Print(bot),
            MoveTo((bx + 2 + shown.chars().count()) as u16, (by + 1) as u16),
            SetCursorStyle::SteadyBar,
            Show
        )
    }
}

/// How long the normal-mode cursor stays visible after a keypress.
const CURSOR_SHOW: Duration = Duration::from_secs(5);

/// While idle, how often the hidden-cursor state is re-asserted. A recovered
/// RACE session replays a screen snapshot that may include a visible cursor;
/// the periodic Hide erases it even when recovery delivers no event.
const HIDE_HEARTBEAT: Duration = Duration::from_secs(1);

/// Hide the cursor and park it in the top-right corner, so a session
/// snapshot that restores with a visible cursor shows it out of the way.
fn park_hidden_cursor(out: &mut impl Write) -> io::Result<()> {
    let (w, _) = terminal::size()?;
    execute!(out, MoveTo(w.saturating_sub(1), 0), Hide)
}

fn run(editor: &mut Editor) -> io::Result<()> {
    // Buffer each frame and flush it as a single write; the default stdout
    // handle would push every queued escape sequence through its own small
    // line buffer.
    let mut out = BufWriter::new(io::stdout());
    loop {
        editor.draw(&mut out)?;
        if editor.mode == Mode::Normal
            && editor.prompt.is_none()
            && editor.show_cursor
            && !event::poll(CURSOR_SHOW)?
        {
            editor.show_cursor = false;
            park_hidden_cursor(&mut out)?;
        }
        // Wait for the next event, re-asserting Hide once per second while
        // the cursor should be invisible.
        while !event::poll(HIDE_HEARTBEAT)? {
            if editor.mode == Mode::Normal
                && editor.prompt.is_none()
                && !editor.show_cursor
            {
                park_hidden_cursor(&mut out)?;
            }
        }
        match event::read()? {
            Event::Key(key) if key.kind != event::KeyEventKind::Release => {
                editor.handle_key(key)?;
            }
            // A recovered/reattached session (RACE) replays the saved screen
            // with the cursor visible; repaint with the cursor hidden again.
            Event::FocusGained | Event::Resize(..) => {
                editor.show_cursor = false;
            }
            _ => {}
        }
        // Drain every already-queued event before redrawing, so fast typing
        // and pastes cost one repaint per batch instead of one per key.
        while !editor.quit && event::poll(Duration::ZERO)? {
            match event::read()? {
                Event::Key(key)
                    if key.kind != event::KeyEventKind::Release =>
                {
                    editor.handle_key(key)?;
                }
                Event::FocusGained | Event::Resize(..) => {
                    editor.show_cursor = false;
                }
                _ => {}
            }
        }
        editor.autosave()?;
        if editor.quit {
            return Ok(());
        }
    }
}

fn main() -> io::Result<()> {
    let mut block = false;
    let mut path = None;
    for arg in env::args().skip(1) {
        if arg == "--block" {
            block = true;
        } else {
            path = Some(PathBuf::from(arg));
        }
    }
    let mut editor = Editor::new(path, block)?;

    terminal::enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableFocusChange)?;
    let result = run(&mut editor);
    execute!(
        io::stdout(),
        SetCursorStyle::DefaultUserShape,
        DisableFocusChange,
        LeaveAlternateScreen
    )?;
    terminal::disable_raw_mode()?;
    result
}
