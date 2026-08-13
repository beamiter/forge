//! ansi — small ANSI helpers used to build the plain-text shadow
//! (`BlockData.output`) consumed by search / export / "copy block".
//! All SGR→TextTag rendering lives in VTE now; see `block_view/blocks.rs`.

pub(crate) fn skip_osc_sequence(bytes: &[u8], mut i: usize) -> usize {
    i += 2;
    while i < bytes.len() {
        if bytes[i] == 0x07 {
            return i + 1;
        }
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
            return i + 2;
        }
        i += 1;
    }
    i
}

pub(crate) fn skip_escape_sequence(bytes: &[u8], i: usize) -> usize {
    if i + 1 >= bytes.len() {
        return i + 1;
    }

    match bytes[i + 1] {
        b'[' => {
            let mut j = i + 2;
            while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                j += 1;
            }
            if j < bytes.len() {
                j += 1;
            }
            j
        }
        b']' => skip_osc_sequence(bytes, i),
        next if (0x20..=0x2f).contains(&next) => {
            let mut j = i + 2;
            while j < bytes.len() && (0x20..=0x2f).contains(&bytes[j]) {
                j += 1;
            }
            if j < bytes.len() && (0x30..=0x7e).contains(&bytes[j]) {
                j += 1;
            }
            j
        }
        _ => i + 2,
    }
}

/// Independent bounds for terminal-replay state. A row/column bound alone is
/// insufficient: a short stream can visit every row at the final column and
/// force `MAX_GRID_ROWS * MAX_GRID_COLS` padded cells into memory. The aggregate
/// cell budget keeps both the plain and styled replay grids bounded regardless
/// of cursor-movement patterns.
const MAX_GRID_ROWS: usize = 100_000;
const MAX_GRID_COLS: usize = 10_000;
const MAX_REPLAY_CELLS: usize = 1024 * 1024;
const MAX_REPLAY_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_CSI_PARAM_BYTES: usize = 256;

/// A row-oriented terminal snapshot with aggregate live and retained-cell
/// budgets. Empty row descriptors are separately bounded by `MAX_GRID_ROWS`;
/// all growth goes through `ensure_len`/`resize_row`, so sparse writes cannot
/// bypass `MAX_REPLAY_CELLS`. Per-row high-water marks also prevent erase/refill
/// cycles from hiding memory retained in `Vec` capacities from the budget.
struct ReplayGrid<T> {
    rows: Vec<Vec<T>>,
    row_cell_high_water: Vec<usize>,
    cells: usize,
    retained_cells: usize,
}

impl<T> ReplayGrid<T> {
    fn new() -> Self {
        Self {
            rows: vec![Vec::new()],
            row_cell_high_water: vec![0],
            cells: 0,
            retained_cells: 0,
        }
    }

    fn ensure_row(&mut self, row: usize) -> bool {
        if row >= MAX_GRID_ROWS {
            return false;
        }
        if self.rows.len() <= row {
            self.rows.resize_with(row + 1, Vec::new);
            self.row_cell_high_water.resize(row + 1, 0);
        }
        true
    }

    fn clear(&mut self) {
        self.rows.clear();
        self.rows.push(Vec::new());
        self.row_cell_high_water.clear();
        self.row_cell_high_water.push(0);
        self.cells = 0;
        self.retained_cells = 0;
    }

    fn clear_row(&mut self, row: usize) {
        let Some(cells) = self.rows.get_mut(row) else {
            return;
        };
        self.cells = self.cells.saturating_sub(cells.len());
        cells.clear();
    }

    fn clear_rows_before(&mut self, end: usize) {
        for cells in self.rows.iter_mut().take(end) {
            self.cells = self.cells.saturating_sub(cells.len());
            cells.clear();
        }
    }

    fn truncate_row(&mut self, row: usize, len: usize) {
        let Some(cells) = self.rows.get_mut(row) else {
            return;
        };
        if cells.len() > len {
            self.cells -= cells.len() - len;
            cells.truncate(len);
        }
    }

    fn truncate_rows(&mut self, len: usize) {
        if self.rows.len() <= len {
            return;
        }
        let removed = self.rows[len..].iter().map(Vec::len).sum::<usize>();
        let removed_retained = self.row_cell_high_water[len..].iter().sum::<usize>();
        self.cells = self.cells.saturating_sub(removed);
        self.retained_cells = self.retained_cells.saturating_sub(removed_retained);
        self.rows.truncate(len);
        self.row_cell_high_water.truncate(len);
    }

    fn pop_row(&mut self) {
        if let Some(row) = self.rows.pop() {
            self.cells = self.cells.saturating_sub(row.len());
            self.retained_cells = self
                .retained_cells
                .saturating_sub(self.row_cell_high_water.pop().unwrap_or(0));
        }
    }
}

impl<T: Clone> ReplayGrid<T> {
    fn ensure_len(&mut self, row: usize, len: usize, fill: T) -> bool {
        if row >= MAX_GRID_ROWS || len > MAX_GRID_COLS {
            return false;
        }
        let old_len = self.rows.get(row).map_or(0, Vec::len);
        let old_high_water = self.row_cell_high_water.get(row).copied().unwrap_or(0);
        let additional = len.saturating_sub(old_len);
        let retained_additional = len.saturating_sub(old_high_water);
        if additional > MAX_REPLAY_CELLS.saturating_sub(self.cells)
            || retained_additional > MAX_REPLAY_CELLS.saturating_sub(self.retained_cells)
        {
            return false;
        }
        if !self.ensure_row(row) {
            return false;
        }
        if additional > 0 {
            if self.rows[row].capacity() < len
                && self.rows[row].try_reserve_exact(additional).is_err()
            {
                return false;
            }
            self.rows[row].resize(len, fill);
            self.cells += additional;
        }
        if retained_additional > 0 {
            self.row_cell_high_water[row] = len;
            self.retained_cells += retained_additional;
        }
        true
    }

    fn resize_row(&mut self, row: usize, len: usize, fill: T) -> bool {
        if row >= MAX_GRID_ROWS || len > MAX_GRID_COLS {
            return false;
        }
        let old_len = self.rows.get(row).map_or(0, Vec::len);
        let old_high_water = self.row_cell_high_water.get(row).copied().unwrap_or(0);
        let cells_without_row = self.cells - old_len;
        let retained_additional = len.saturating_sub(old_high_water);
        if len > MAX_REPLAY_CELLS.saturating_sub(cells_without_row)
            || retained_additional > MAX_REPLAY_CELLS.saturating_sub(self.retained_cells)
        {
            return false;
        }
        if !self.ensure_row(row) {
            return false;
        }
        if self.rows[row].capacity() < len
            && self.rows[row]
                .try_reserve_exact(len.saturating_sub(old_len))
                .is_err()
        {
            return false;
        }
        self.rows[row].resize(len, fill);
        self.cells = cells_without_row + len;
        if retained_additional > 0 {
            self.row_cell_high_water[row] = len;
            self.retained_cells += retained_additional;
        }
        true
    }

    fn set_cell(&mut self, row: usize, col: usize, value: T, fill: T) -> bool {
        let Some(len) = col.checked_add(1) else {
            return false;
        };
        if !self.ensure_len(row, len, fill) {
            return false;
        }
        self.rows[row][col] = value;
        true
    }
}

fn bounded_utf8_prefix(input: &str, max_bytes: usize) -> &str {
    let mut end = input.len().min(max_bytes);
    while !input.is_char_boundary(end) {
        end -= 1;
    }
    &input[..end]
}

/// Reconstruct the on-screen text a stream of bytes would leave behind, applying
/// a full two-dimensional cursor model (home/absolute positioning, vertical
/// moves, and screen clears) in addition to the horizontal CR/erase semantics.
///
/// This matters for programs that repaint in place without switching to the
/// alternate screen — `top`, `watch`, and multi-line progress UIs emit
/// cursor-home (`\x1b[H`) before every frame. A horizontal-only model treats
/// each frame's lines as fresh output and concatenates them, so a long `top`
/// session grows an unbounded block. Modelling vertical position collapses the
/// repeated frames down to the final one, matching what the live VTE showed.
///
/// The `bool` reports whether a full-screen clear (`\x1b[2J`/`\x1b[3J`) was
/// seen — retained for callers that key off it.
fn next_lossy_char(bytes: &[u8], i: usize) -> (char, usize) {
    let first = bytes[i];
    if first.is_ascii() {
        return (char::from(first), i + 1);
    }

    let width = match first {
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return ('\u{fffd}', i + 1),
    };
    let end = i.saturating_add(width);
    if end > bytes.len() {
        return ('\u{fffd}', i + 1);
    }
    match std::str::from_utf8(&bytes[i..end]) {
        Ok(scalar) => (scalar.chars().next().unwrap_or('\u{fffd}'), end),
        Err(_) => ('\u{fffd}', i + 1),
    }
}

fn replay_plain_ansi_bytes(bytes: &[u8]) -> (ReplayGrid<char>, bool) {
    let mut grid = ReplayGrid::<char>::new();
    let mut row = 0usize;
    let mut col = 0usize;
    let mut i = 0;
    let mut should_clear = false;

    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'[' => {
                    i += 2;
                    let params_start = i;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    if i < bytes.len() {
                        let final_byte = bytes[i];
                        let params = &bytes[params_start..i];
                        i += 1;

                        // CSI parameters are normally a handful of bytes. Ignore
                        // absurd runs so downstream parsing (notably styled SGR)
                        // cannot turn a bounded input into a large temporary.
                        if params.len() > MAX_CSI_PARAM_BYTES {
                            continue;
                        }

                        // Private-mode sequences (`\x1b[?…`, `\x1b[>…`) never move
                        // the cursor or edit cells in this shadow; skip them so a
                        // trailing `h`/`l` is not mistaken for a real command.
                        if matches!(params.first(), Some(0x3c..=0x3f)) {
                            continue;
                        }

                        match final_byte {
                            b'H' | b'f' => {
                                let r = parse_param_nth(params, 0, 1);
                                let c = parse_param_nth(params, 1, 1);
                                row = r.saturating_sub(1).min(MAX_GRID_ROWS);
                                col = c.saturating_sub(1).min(MAX_GRID_COLS);
                            }
                            b'A' => {
                                let n = parse_param_first(params, 1);
                                row = row.saturating_sub(n);
                            }
                            b'B' | b'e' => {
                                let n = parse_param_first(params, 1);
                                row = row.saturating_add(n).min(MAX_GRID_ROWS);
                            }
                            b'E' => {
                                let n = parse_param_first(params, 1);
                                row = row.saturating_add(n).min(MAX_GRID_ROWS);
                                col = 0;
                            }
                            b'F' => {
                                let n = parse_param_first(params, 1);
                                row = row.saturating_sub(n);
                                col = 0;
                            }
                            b'd' => {
                                let r = parse_param_first(params, 1);
                                row = r.saturating_sub(1).min(MAX_GRID_ROWS);
                            }
                            b'C' | b'a' => {
                                let n = parse_param_first(params, 1);
                                col = col.saturating_add(n).min(MAX_GRID_COLS);
                            }
                            b'D' => {
                                let n = parse_param_first(params, 1);
                                col = col.saturating_sub(n);
                            }
                            b'G' | b'`' => {
                                let c = parse_param_first(params, 1);
                                col = c.saturating_sub(1).min(MAX_GRID_COLS);
                            }
                            b'J' => match params {
                                b"" | b"0" => {
                                    // Erase from the cursor to the end of screen.
                                    grid.truncate_row(row, col);
                                    grid.truncate_rows(row.saturating_add(1));
                                }
                                b"1" => {
                                    // Erase from the start of screen up to the cursor.
                                    grid.clear_rows_before(row);
                                    if let Some(cells) = grid.rows.get_mut(row) {
                                        let upto = col.min(cells.len());
                                        for c in cells.iter_mut().take(upto) {
                                            *c = ' ';
                                        }
                                    }
                                }
                                b"2" | b"3" => {
                                    should_clear = true;
                                    grid.clear();
                                    row = 0;
                                    col = 0;
                                }
                                _ => {}
                            },
                            b'K' => match params {
                                b"" | b"0" => {
                                    grid.truncate_row(row, col);
                                }
                                b"1" => {
                                    if let Some(cells) = grid.rows.get_mut(row) {
                                        let upto = col.min(cells.len());
                                        for c in cells.iter_mut().take(upto) {
                                            *c = ' ';
                                        }
                                    }
                                }
                                b"2" => grid.clear_row(row),
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                }
                b']' => {
                    i = skip_osc_sequence(bytes, i);
                }
                _ => {
                    i = skip_escape_sequence(bytes, i);
                }
            }
        } else if bytes[i] == b'\n' {
            row = row.saturating_add(1).min(MAX_GRID_ROWS);
            col = 0;
            // Materialise the destination row so a trailing newline still emits
            // its blank line, matching a real terminal's line feed.
            if !grid.ensure_row(row) {
                break;
            }
            i += 1;
        } else if bytes[i] == b'\r' {
            col = 0;
            i += 1;
        } else if bytes[i] == b'\x08' {
            col = col.saturating_sub(1);
            i += 1;
        } else {
            let (ch, next) = next_lossy_char(bytes, i);
            i = next;
            if col >= MAX_GRID_COLS || !grid.set_cell(row, col, ch, ' ') {
                break;
            }
            col = col.saturating_add(1);
        }
    }
    (grid, should_clear)
}

fn replay_plain_ansi(input: &str) -> (ReplayGrid<char>, bool) {
    replay_plain_ansi_bytes(input.as_bytes())
}

pub(crate) fn strip_ansi_with_clear_detect(input: &str) -> (String, bool) {
    let bytes = input.as_bytes();
    if memchr::memchr3(0x1b, b'\r', b'\x08', bytes).is_none() {
        return (
            bounded_utf8_prefix(input, MAX_REPLAY_OUTPUT_BYTES).to_owned(),
            false,
        );
    }

    let (grid, should_clear) = replay_plain_ansi(input);
    let mut result = String::with_capacity(input.len().min(MAX_REPLAY_OUTPUT_BYTES));
    'rows: for (line_idx, cells) in grid.rows.iter().enumerate() {
        if line_idx > 0 {
            if result.len() == MAX_REPLAY_OUTPUT_BYTES {
                break;
            }
            result.push('\n');
        }
        for &ch in cells {
            if ch.len_utf8() > MAX_REPLAY_OUTPUT_BYTES.saturating_sub(result.len()) {
                break 'rows;
            }
            result.push(ch);
        }
    }
    (result, should_clear)
}

/// Whether replaying `input` leaves any visible, non-whitespace cell.
///
/// Unlike [`strip_ansi_with_clear_detect`], this never serialises a plain-text
/// copy. The common no-control path is a direct character scan; cursor motion
/// and erases reuse the same bounded replay grid as final Block rendering.
pub(crate) fn ansi_has_visible_text(input: &str) -> bool {
    ansi_bytes_have_visible_text(input.as_bytes())
}

/// Byte-oriented visibility check for raw PTY capture.
///
/// This deliberately performs lossy UTF-8 decoding a scalar at a time instead
/// of calling `String::from_utf8_lossy`: malformed output must not make the
/// metadata-only Unified finalize path allocate and copy the whole capture.
pub(crate) fn ansi_bytes_have_visible_text(bytes: &[u8]) -> bool {
    if memchr::memchr3(0x1b, b'\r', b'\x08', bytes).is_none() {
        let mut i = 0;
        while i < bytes.len() {
            let (ch, next) = next_lossy_char(bytes, i);
            if !ch.is_whitespace() && !ch.is_control() {
                return true;
            }
            i = next;
        }
        return false;
    }

    let (grid, _) = replay_plain_ansi_bytes(bytes);
    grid.rows
        .iter()
        .flatten()
        .any(|ch| !ch.is_whitespace() && !ch.is_control())
}

/// Whether a captured output stream repaints in place using vertical cursor
/// motion — the fingerprint of a full-screen program that does *not* switch to
/// the alternate screen (`top`, `watch`, multi-line progress bars). Such a
/// stream must be collapsed to its final frame before it feeds a scrollback-
/// backed VTE, or every frame stacks. A lone leading `\x1b[H` (some tools emit
/// one before ordinary output) is deliberately not counted: repaint sequences
/// only matter once real lines already exist above the cursor.
pub(crate) fn output_has_vertical_repaint(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut seen_newline = false;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                seen_newline = true;
                i += 1;
            }
            0x1b if i + 1 < bytes.len() => match bytes[i + 1] {
                b'[' => {
                    let params_start = i + 2;
                    let mut j = params_start;
                    while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                        j += 1;
                    }
                    if j >= bytes.len() {
                        return false;
                    }
                    let is_private = matches!(bytes.get(params_start), Some(0x3c..=0x3f));
                    if seen_newline && !is_private {
                        match bytes[j] {
                            b'A' | b'H' | b'f' | b'd' | b'F' => return true,
                            b'J' => {
                                let params = &bytes[params_start..j];
                                if matches!(params, b"1" | b"2" | b"3") {
                                    return true;
                                }
                            }
                            _ => {}
                        }
                    }
                    i = j + 1;
                }
                b']' => i = skip_osc_sequence(bytes, i),
                _ => i = skip_escape_sequence(bytes, i),
            },
            _ => i += 1,
        }
    }
    false
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SgrColor {
    Default,
    Palette(u8),
    Rgb(u8, u8, u8),
}

/// The subset of SGR state a repaint snapshot needs to reproduce. `top`'s look
/// (bold values, reverse-video header/footer bars, colored columns) is entirely
/// these attributes plus the 16-colour and 256/truecolour palettes.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Sgr {
    bold: bool,
    dim: bool,
    italic: bool,
    underline: bool,
    blink: bool,
    reverse: bool,
    hidden: bool,
    strike: bool,
    fg: SgrColor,
    bg: SgrColor,
}

impl Default for Sgr {
    fn default() -> Self {
        Sgr {
            bold: false,
            dim: false,
            italic: false,
            underline: false,
            blink: false,
            reverse: false,
            hidden: false,
            strike: false,
            fg: SgrColor::Default,
            bg: SgrColor::Default,
        }
    }
}

/// Apply an SGR parameter list (`\x1b[…m`) to the running attribute state.
fn apply_sgr(cur: &mut Sgr, params: &[u8]) {
    if params.is_empty() {
        *cur = Sgr::default();
        return;
    }
    let parts: Vec<&[u8]> = params.split(|&b| b == b';').collect();
    let num = |p: &[u8]| -> usize {
        let mut v = 0usize;
        for &b in p {
            if b.is_ascii_digit() {
                v = v.saturating_mul(10).saturating_add((b - b'0') as usize);
            } else {
                return usize::MAX;
            }
        }
        v
    };
    let mut i = 0;
    while i < parts.len() {
        match num(parts[i]) {
            0 | usize::MAX => *cur = Sgr::default(), // bare/`;`/garbage → reset
            1 => cur.bold = true,
            2 => cur.dim = true,
            3 => cur.italic = true,
            4 => cur.underline = true,
            5 | 6 => cur.blink = true,
            7 => cur.reverse = true,
            8 => cur.hidden = true,
            9 => cur.strike = true,
            21 | 22 => {
                cur.bold = false;
                cur.dim = false;
            }
            23 => cur.italic = false,
            24 => cur.underline = false,
            25 => cur.blink = false,
            27 => cur.reverse = false,
            28 => cur.hidden = false,
            29 => cur.strike = false,
            n @ 30..=37 => cur.fg = SgrColor::Palette((n - 30) as u8),
            39 => cur.fg = SgrColor::Default,
            n @ 40..=47 => cur.bg = SgrColor::Palette((n - 40) as u8),
            49 => cur.bg = SgrColor::Default,
            n @ 90..=97 => cur.fg = SgrColor::Palette((n - 90 + 8) as u8),
            n @ 100..=107 => cur.bg = SgrColor::Palette((n - 100 + 8) as u8),
            sel @ (38 | 48) => {
                // Extended colour: `38;5;n` (256) or `38;2;r;g;b` (truecolour).
                let mode = parts.get(i + 1).map(|p| num(p)).unwrap_or(usize::MAX);
                let color = if mode == 5 {
                    let idx = parts.get(i + 2).map(|p| num(p)).unwrap_or(0);
                    i += 2;
                    SgrColor::Palette(idx.min(255) as u8)
                } else if mode == 2 {
                    let c = |k: usize| parts.get(i + k).map(|p| num(p)).unwrap_or(0).min(255) as u8;
                    let rgb = SgrColor::Rgb(c(2), c(3), c(4));
                    i += 4;
                    rgb
                } else {
                    SgrColor::Default
                };
                if sel == 38 {
                    cur.fg = color;
                } else {
                    cur.bg = color;
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// Serialise attribute state to SGR parameters, always rebuilt from a `0` reset
/// so each transition is self-contained.
fn sgr_codes(s: &Sgr) -> String {
    let mut v: Vec<String> = vec!["0".into()];
    for (on, code) in [
        (s.bold, "1"),
        (s.dim, "2"),
        (s.italic, "3"),
        (s.underline, "4"),
        (s.blink, "5"),
        (s.reverse, "7"),
        (s.hidden, "8"),
        (s.strike, "9"),
    ] {
        if on {
            v.push(code.into());
        }
    }
    let mut push_color = |c: SgrColor, base: usize, ext: usize| match c {
        SgrColor::Default => {}
        SgrColor::Palette(i) if i < 8 => v.push((base + i as usize).to_string()),
        SgrColor::Palette(i) if i < 16 => v.push((base + 60 + (i as usize - 8)).to_string()),
        SgrColor::Palette(i) => {
            v.push(ext.to_string());
            v.push("5".into());
            v.push(i.to_string());
        }
        SgrColor::Rgb(r, g, b) => {
            v.push(ext.to_string());
            v.push("2".into());
            v.push(r.to_string());
            v.push(g.to_string());
            v.push(b.to_string());
        }
    };
    push_color(s.fg, 30, 38);
    push_color(s.bg, 40, 48);
    v.join(";")
}

/// Collapse a full-screen repaint stream (see [`output_has_vertical_repaint`])
/// to a single clean frame, **preserving colour**.
///
/// The whole stream is replayed through a 2D screen model that stores each
/// cell's character *and* SGR attributes. `top` and friends repaint
/// *incrementally* — a refresh rewrites only the lines that changed (clock,
/// CPU%, times) and leaves static rows untouched — so accumulating every write
/// reconstructs the final screen exactly; isolating a single frame would drop
/// the unchanged rows. `\x1b[K`/`\x1b[J` erases fill with the active background,
/// which is how top's reverse-video header/footer bars extend to the screen
/// edge, so those are reproduced too.
///
/// The result is serialised with `\r\n` line breaks (a finished VTE treats a
/// bare `\n` as line-feed only, which would stair-step the frame) and minimal
/// SGR transitions. Trailing blank rows and trailing *default-styled* blank
/// cells are trimmed (coloured trailing cells — the bars — are kept), so the
/// snapshot is tight without wrapping at the VTE's right edge.
pub(crate) fn collapse_repaint_output(input: &str, cols: usize) -> String {
    let bytes = input.as_bytes();
    let width = cols.clamp(1, MAX_GRID_COLS);
    let blank = (' ', Sgr::default());

    let mut grid = ReplayGrid::<(char, Sgr)>::new();
    let mut row = 0usize;
    let mut col = 0usize;
    let mut cur = Sgr::default();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() {
            match bytes[i + 1] {
                b'[' => {
                    i += 2;
                    let ps = i;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    if i >= bytes.len() {
                        break;
                    }
                    let final_byte = bytes[i];
                    let params = &bytes[ps..i];
                    i += 1;
                    if params.len() > MAX_CSI_PARAM_BYTES {
                        continue;
                    }
                    if matches!(params.first(), Some(0x3c..=0x3f)) {
                        continue; // private mode — no cursor/text effect here
                    }
                    match final_byte {
                        b'm' => apply_sgr(&mut cur, params),
                        b'H' | b'f' => {
                            row = parse_param_nth(params, 0, 1)
                                .saturating_sub(1)
                                .min(MAX_GRID_ROWS);
                            col = parse_param_nth(params, 1, 1).saturating_sub(1).min(width);
                        }
                        b'A' => row = row.saturating_sub(parse_param_first(params, 1)),
                        b'B' | b'e' => {
                            row = row
                                .saturating_add(parse_param_first(params, 1))
                                .min(MAX_GRID_ROWS)
                        }
                        b'E' => {
                            row = row
                                .saturating_add(parse_param_first(params, 1))
                                .min(MAX_GRID_ROWS);
                            col = 0;
                        }
                        b'F' => {
                            row = row.saturating_sub(parse_param_first(params, 1));
                            col = 0;
                        }
                        b'd' => {
                            row = parse_param_first(params, 1)
                                .saturating_sub(1)
                                .min(MAX_GRID_ROWS)
                        }
                        b'C' | b'a' => {
                            col = col.saturating_add(parse_param_first(params, 1)).min(width)
                        }
                        b'D' => col = col.saturating_sub(parse_param_first(params, 1)),
                        b'G' | b'`' => {
                            col = parse_param_first(params, 1).saturating_sub(1).min(width)
                        }
                        b'K' => {
                            let fill = (' ', cur);
                            match params {
                                b"" | b"0" => {
                                    grid.truncate_row(row, col);
                                    if !grid.resize_row(row, width, fill) {
                                        break;
                                    }
                                    if let Some(cells) = grid.rows.get_mut(row) {
                                        for cell in cells.iter_mut().skip(col) {
                                            *cell = fill;
                                        }
                                    }
                                }
                                b"1" => {
                                    if !grid.ensure_len(row, col, blank) {
                                        break;
                                    }
                                    if let Some(cells) = grid.rows.get_mut(row) {
                                        for cell in cells.iter_mut().take(col) {
                                            *cell = fill;
                                        }
                                    }
                                }
                                b"2" => {
                                    if !grid.resize_row(row, width, fill) {
                                        break;
                                    }
                                    if let Some(cells) = grid.rows.get_mut(row) {
                                        cells.fill(fill);
                                    }
                                }
                                _ => {}
                            }
                        }
                        b'J' => match params {
                            b"" | b"0" => {
                                grid.truncate_rows(row.saturating_add(1));
                                grid.truncate_row(row, col);
                                if !grid.resize_row(row, width, (' ', cur)) {
                                    break;
                                }
                                if let Some(cells) = grid.rows.get_mut(row) {
                                    for cell in cells.iter_mut().skip(col) {
                                        *cell = (' ', cur);
                                    }
                                }
                            }
                            b"1" => {
                                grid.clear_rows_before(row);
                                if !grid.ensure_len(row, col, blank) {
                                    break;
                                }
                                if let Some(cells) = grid.rows.get_mut(row) {
                                    for cell in cells.iter_mut().take(col) {
                                        *cell = (' ', cur);
                                    }
                                }
                            }
                            b"2" | b"3" => {
                                grid.clear();
                                row = 0;
                                col = 0;
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
                b']' => i = skip_osc_sequence(bytes, i),
                _ => i = skip_escape_sequence(bytes, i),
            }
        } else if bytes[i] == b'\n' {
            row = row.saturating_add(1).min(MAX_GRID_ROWS);
            col = 0;
            if !grid.ensure_row(row) {
                break;
            }
            i += 1;
        } else if bytes[i] == b'\r' {
            col = 0;
            i += 1;
        } else if bytes[i] == b'\x08' {
            col = col.saturating_sub(1);
            i += 1;
        } else {
            let ch = input[i..].chars().next().unwrap_or('\u{FFFD}');
            i += ch.len_utf8();
            // Preserve the existing simplified right-edge behaviour: bytes past
            // the terminal width are ignored until CR/LF repositions the cursor.
            // `width` is a real terminal dimension, not a security-budget hit.
            if col < width && !grid.set_cell(row, col, (ch, cur), blank) {
                break;
            }
            col = col.saturating_add(1).min(width);
        }
    }

    // Drop screen-padding rows at the bottom that carry no visible content.
    let is_blank_row =
        |r: &[(char, Sgr)]| r.iter().all(|&(ch, s)| ch == ' ' && s == Sgr::default());
    while grid.rows.len() > 1 && grid.rows.last().is_some_and(|r| is_blank_row(r)) {
        grid.pop_row();
    }

    const ANSI_RESET: &str = "\x1b[0m";
    let mut out = String::with_capacity(input.len().min(64 * 1024).min(MAX_REPLAY_OUTPUT_BYTES));
    'rows: for (ri, cells) in grid.rows.iter().enumerate() {
        if ri > 0 {
            if "\r\n".len() > MAX_REPLAY_OUTPUT_BYTES.saturating_sub(out.len()) {
                break;
            }
            out.push_str("\r\n");
        }
        // Keep coloured trailing cells (the bars); trim default-styled padding.
        let mut end = cells.len();
        while end > 0 {
            let (ch, s) = cells[end - 1];
            if ch == ' ' && s == Sgr::default() {
                end -= 1;
            } else {
                break;
            }
        }
        let mut emitted = Sgr::default();
        for &(ch, s) in &cells[..end] {
            let codes = (s != emitted).then(|| sgr_codes(&s));
            let transition_bytes = codes.as_ref().map_or(0, |codes| 3 + codes.len());
            // If this cell leaves a non-default style active, reserve room for
            // the reset as part of the same atomic output unit. Truncation can
            // therefore never leak an unfinished SGR state into the VTE.
            let reset_bytes = if s == Sgr::default() {
                0
            } else {
                ANSI_RESET.len()
            };
            let required = transition_bytes
                .saturating_add(ch.len_utf8())
                .saturating_add(reset_bytes);
            if required > MAX_REPLAY_OUTPUT_BYTES.saturating_sub(out.len()) {
                if emitted != Sgr::default()
                    && ANSI_RESET.len() <= MAX_REPLAY_OUTPUT_BYTES.saturating_sub(out.len())
                {
                    out.push_str(ANSI_RESET);
                }
                break 'rows;
            }
            if let Some(codes) = codes {
                out.push_str("\x1b[");
                out.push_str(&codes);
                out.push('m');
                emitted = s;
            }
            out.push(ch);
        }
        if emitted != Sgr::default() {
            out.push_str(ANSI_RESET);
        }
    }
    out
}

pub(crate) fn contains_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    let first = needle[0];
    let finder = memchr::memchr_iter(first, haystack);
    for pos in finder {
        // memchr_iter yields ascending positions, so once a candidate would run
        // past the end every later one does too: stop scanning this byte, but
        // still fall through to the uppercase-variant search below.
        if pos + needle.len() > haystack.len() {
            break;
        }
        let candidate = &haystack[pos..pos + needle.len()];
        if candidate
            .iter()
            .zip(needle.iter())
            .all(|(&h, &n)| h.to_ascii_lowercase() == n)
        {
            return true;
        }
    }
    // Also check uppercase variant of first byte
    let first_upper = first.to_ascii_uppercase();
    if first_upper != first {
        let finder = memchr::memchr_iter(first_upper, haystack);
        for pos in finder {
            if pos + needle.len() > haystack.len() {
                break;
            }
            let candidate = &haystack[pos..pos + needle.len()];
            if candidate
                .iter()
                .zip(needle.iter())
                .all(|(&h, &n)| h.to_ascii_lowercase() == n)
            {
                return true;
            }
        }
    }
    false
}

pub(crate) fn parse_param_first(buf: &[u8], default: usize) -> usize {
    parse_param_nth(buf, 0, default)
}

/// Parse the `n`-th semicolon-separated numeric parameter, saturating rather
/// than overflowing on absurdly long digit runs. An empty or non-numeric field
/// yields `default`.
pub(crate) fn parse_param_nth(buf: &[u8], n: usize, default: usize) -> usize {
    let Some(field) = buf.split(|&b| b == b';').nth(n) else {
        return default;
    };
    if field.is_empty() {
        return default;
    }
    let mut val = 0usize;
    for &b in field {
        if b.is_ascii_digit() {
            val = val.saturating_mul(10).saturating_add((b - b'0') as usize);
        } else {
            return default;
        }
    }
    if val == 0 {
        default
    } else {
        val
    }
}

pub(crate) fn strip_ansi(input: &str) -> String {
    strip_ansi_with_clear_detect(input).0
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::{
        ansi_bytes_have_visible_text, bounded_utf8_prefix, collapse_repaint_output,
        contains_case_insensitive as cci, strip_ansi_with_clear_detect, ReplayGrid,
        MAX_CSI_PARAM_BYTES, MAX_GRID_COLS, MAX_REPLAY_CELLS, MAX_REPLAY_OUTPUT_BYTES,
    };

    #[test]
    fn byte_visibility_decodes_malformed_utf8_without_a_lossy_string() {
        assert!(ansi_bytes_have_visible_text(b"\xff"));
        assert!(!ansi_bytes_have_visible_text(b"\xff\r\x1b[2K"));
        assert!(ansi_bytes_have_visible_text("\u{3000}\u{754c}".as_bytes()));
        assert!(!ansi_bytes_have_visible_text("\u{3000}\n".as_bytes()));
    }

    #[test]
    fn matches_regardless_of_case() {
        assert!(cci(b"Hello World", b"hello"));
        assert!(cci(b"hello world", b"world"));
        assert!(cci(b"MiXeD", b"mixed"));
    }

    #[test]
    fn reports_absent_needle() {
        assert!(!cci(b"abcdef", b"xyz"));
    }

    #[test]
    fn empty_needle_always_matches() {
        assert!(cci(b"anything", b""));
    }

    #[test]
    fn finds_uppercase_first_byte_after_oob_lowercase_hit() {
        // Regression: the lowercase-first-byte scan finds 'f' at the final
        // position (out of bounds for a 2-byte needle). The old code returned
        // false there, skipping the uppercase-variant scan that matches "Fo"
        // at position 0.
        assert!(cci(b"Foxf", b"fo"));
    }

    #[test]
    fn strip_plain_multiline_output_uses_passthrough_result() {
        assert_eq!(
            strip_ansi_with_clear_detect("plain\ntext\n"),
            ("plain\ntext\n".to_string(), false)
        );
    }

    #[test]
    fn bounded_prefix_never_splits_utf8() {
        assert_eq!(bounded_utf8_prefix("ab€cd", 4), "ab");
        assert_eq!(bounded_utf8_prefix("ab€cd", 5), "ab€");
        assert_eq!(bounded_utf8_prefix("ab€cd", usize::MAX), "ab€cd");
    }

    #[test]
    fn replay_grid_enforces_aggregate_cell_budget_and_reclaims_it() {
        let mut grid = ReplayGrid::<char>::new();
        let full_rows = MAX_REPLAY_CELLS / MAX_GRID_COLS;
        for row in 0..full_rows {
            assert!(grid.ensure_len(row, MAX_GRID_COLS, ' '));
        }
        assert_eq!(grid.cells, full_rows * MAX_GRID_COLS);
        assert!(!grid.ensure_len(full_rows, MAX_GRID_COLS, ' '));
        assert!(grid.cells <= MAX_REPLAY_CELLS);

        grid.clear();
        for row in 0..full_rows {
            assert!(grid.ensure_len(row, MAX_GRID_COLS, ' '));
            grid.clear_row(row);
        }
        assert_eq!(grid.cells, 0);
        assert_eq!(grid.retained_cells, full_rows * MAX_GRID_COLS);
        assert!(!grid.ensure_len(full_rows, MAX_GRID_COLS, ' '));

        // A full screen reset drops every inner allocation and may safely make
        // the budget available again.
        grid.clear();
        assert!(grid.ensure_len(0, MAX_GRID_COLS, ' '));
        assert_eq!(grid.cells, MAX_GRID_COLS);
    }

    fn sparse_last_column_stream() -> String {
        let mut input = String::new();
        let attempted_rows = MAX_REPLAY_CELLS / MAX_GRID_COLS + 2;
        for row in 1..=attempted_rows {
            write!(input, "\x1b[{row};{MAX_GRID_COLS}H#").unwrap();
        }
        input
    }

    #[test]
    fn sparse_cursor_writes_cannot_expand_plain_grid_without_bound() {
        let input = sparse_last_column_stream();
        assert!(input.len() < 1024 * 1024);

        let (output, _) = strip_ansi_with_clear_detect(&input);
        let full_rows = MAX_REPLAY_CELLS / MAX_GRID_COLS;
        assert_eq!(
            output.bytes().filter(|&byte| byte == b'#').count(),
            full_rows
        );
        assert!(output.len() <= MAX_REPLAY_CELLS + full_rows);
        assert!(output.len() <= MAX_REPLAY_OUTPUT_BYTES);
    }

    #[test]
    fn sparse_cursor_writes_cannot_expand_styled_grid_without_bound() {
        let input = sparse_last_column_stream();
        let output = collapse_repaint_output(&input, MAX_GRID_COLS);
        let full_rows = MAX_REPLAY_CELLS / MAX_GRID_COLS;
        assert_eq!(
            output.bytes().filter(|&byte| byte == b'#').count(),
            full_rows
        );
        assert!(output.len() <= MAX_REPLAY_CELLS + full_rows * 2);
        assert!(output.len() <= MAX_REPLAY_OUTPUT_BYTES);
    }

    #[test]
    fn saturated_cursor_parameters_do_not_overflow() {
        let digits = "9".repeat(MAX_CSI_PARAM_BYTES);
        assert_eq!(
            strip_ansi_with_clear_detect(&format!("ok\x1b[{digits}Bignored")),
            ("ok".to_string(), false)
        );
        assert_eq!(
            strip_ansi_with_clear_detect(&format!("ok\r\x1b[{digits}Cignored")),
            ("ok".to_string(), false)
        );
    }

    #[test]
    fn oversized_csi_parameter_run_is_ignored() {
        let digits = "9".repeat(MAX_CSI_PARAM_BYTES + 1);
        assert_eq!(
            strip_ansi_with_clear_detect(&format!("a\x1b[{digits}Cb")),
            ("ab".to_string(), false)
        );
    }
}
