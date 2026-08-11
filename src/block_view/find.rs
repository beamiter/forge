//! Bounded find/search state and navigation for Block-mode terminal history.
//!
//! Find-within-blocks: VTE's native PCRE2 highlighter paints every hit inside
//! each finished block's command/output VTE; we only track which (block, surface)
//! each hit belongs to so Next/Prev can step the per-VTE search cursor across
//! block boundaries. Also hosts the metadata-only filter pass used by the
//! command palette's failed/slow toggles and by the debug dashboard counts.

use gtk4::glib;
use gtk4::prelude::*;
use std::time::{Duration, Instant};
use vte4::TerminalExt;

use super::{
    contains_case_insensitive, replace_finished_block_selection, BlockFilters, BlockOutcome,
    TermView,
};

fn outcome_matches_filters(
    resolved_command: &str,
    raw_exit_code: Option<i32>,
    filters: &BlockFilters,
) -> bool {
    let outcome = BlockOutcome::classify(Some(resolved_command), raw_exit_code);
    if filters
        .exit_code
        .is_some_and(|exit_code| outcome.reported_exit_code() != Some(exit_code))
    {
        return false;
    }
    !filters.failed_only || matches!(outcome, BlockOutcome::Failure(_))
}

/// Stop common queries from turning a bounded output history into unbounded
/// match metadata or a long-running main-thread scan. Reaching the limit is
/// deliberately reported as capped even when the retained history happens to
/// contain exactly this many hits: proving equality would require scanning the
/// remainder, defeating the early-stop guarantee.
pub(crate) const FIND_MATCH_LIMIT: usize = 10_000;
const FIND_SCAN_BYTE_LIMIT: usize = 4 * 1024 * 1024;
const FIND_SCAN_TIME_LIMIT: Duration = Duration::from_millis(12);
/// VTE uses PCRE2 while match counting uses Rust's Unicode-aware regex engine.
/// UTF validates/decodes the subject as Unicode and UCP makes shorthand classes
/// such as `\d`, `\s`, and `\w` use Unicode properties on the VTE side too.
const VTE_SEARCH_FLAGS: u32 = pcre2_sys::PCRE2_CASELESS
    | pcre2_sys::PCRE2_MULTILINE
    | pcre2_sys::PCRE2_UTF
    | pcre2_sys::PCRE2_UCP;

/// One searchable VTE surface. VTE owns the exact match positions and paints
/// every occurrence; Forge needs only the number of occurrences on each
/// surface to navigate across block boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FindSurface {
    pub(crate) block_id: u64,
    pub(crate) block_index: usize,
    /// false = command VTE, true = output VTE.
    pub(crate) is_output: bool,
    /// Hit lives in the live VTE (the still-running command's output), not in
    /// any finished block; `block_id`/`block_index` are meaningless then.
    pub(crate) is_live: bool,
    /// Number of occurrences retained for navigation on this surface. Always
    /// positive and bounded by [`FIND_MATCH_LIMIT`].
    pub(crate) count: usize,
    /// Native VTE cursor position last confirmed by a successful search call.
    vte_cursor: Option<usize>,
    /// False when the match or scan budget stopped inside this surface.
    complete: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FindHighlight {
    block_id: u64,
    block_index: usize,
    is_output: bool,
}

impl From<&FindSurface> for FindHighlight {
    fn from(surface: &FindSurface) -> Self {
        Self {
            block_id: surface.block_id,
            block_index: surface.block_index,
            is_output: surface.is_output,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct FindCursor {
    surface: usize,
    occurrence: usize,
    /// Zero-based position across the compressed surface list.
    global: usize,
}

#[derive(Default)]
pub(crate) struct FindState {
    pub(crate) surfaces: Vec<FindSurface>,
    cursor: FindCursor,
    total: usize,
    capped: bool,
    scan_limited: bool,
    /// Highlights installed by the flat cross-block palette do not participate
    /// in incremental navigation, but must still be cleared without walking all
    /// retained blocks on every debounced query.
    extra_highlights: Vec<FindHighlight>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct FindProgress {
    pub(crate) current: usize,
    pub(crate) total: usize,
    pub(crate) capped: bool,
    pub(crate) scan_limited: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FindSearchResult {
    NoMatches,
    InvalidRegex,
    ScanLimit,
    Matches(FindProgress),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FindNavigationResult {
    /// No compressed Block search is active; the UI may use its classic VTE
    /// fallback instead.
    Inactive,
    Progress(FindProgress),
    /// The target block disappeared or VTE could not confirm the expected hit.
    /// The stale Block search has already been cleared.
    Invalidated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FindDirection {
    Next,
    Previous,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeCursorAction {
    AlreadySelected,
    Step { wrap_once: bool },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BoundedMatchCount {
    count: usize,
    reached_limit: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScanPrefix<'a> {
    text: &'a str,
    incomplete: bool,
}

struct FindScanBudget {
    remaining_bytes: usize,
    started: Instant,
}

impl FindScanBudget {
    fn new() -> Self {
        Self {
            remaining_bytes: FIND_SCAN_BYTE_LIMIT,
            started: Instant::now(),
        }
    }

    fn take_prefix<'a>(&mut self, text: &'a str) -> ScanPrefix<'a> {
        if self.time_exhausted() || self.remaining_bytes == 0 {
            return ScanPrefix {
                text: "",
                incomplete: !text.is_empty(),
            };
        }
        let prefix = utf8_prefix(text, self.remaining_bytes);
        self.remaining_bytes = self.remaining_bytes.saturating_sub(prefix.len());
        ScanPrefix {
            text: prefix,
            incomplete: prefix.len() < text.len(),
        }
    }

    fn time_exhausted(&self) -> bool {
        self.started.elapsed() >= FIND_SCAN_TIME_LIMIT
    }

    fn remaining_bytes(&self) -> usize {
        self.remaining_bytes
    }
}

fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RegexConsumption {
    Consuming,
    ZeroWidth,
    Never,
}

/// Classify valid Rust regexes before installing the corresponding PCRE2 regex.
/// Patterns capable of zero-width matches are rejected: VTE and Rust's iterator
/// do not expose compatible cursor semantics for assertions such as `^`, `\b`,
/// or optional/empty repetitions.
fn regex_consumption(pattern: &str) -> Result<RegexConsumption, ()> {
    let hir = regex_syntax::parse(pattern).map_err(|_| ())?;
    match hir.properties().minimum_len() {
        Some(0) => Ok(RegexConsumption::ZeroWidth),
        Some(_) => Ok(RegexConsumption::Consuming),
        None => Ok(RegexConsumption::Never),
    }
}

fn bounded_match_count(
    regex: &regex::Regex,
    haystack: &str,
    remaining: usize,
) -> BoundedMatchCount {
    if remaining == 0 {
        return BoundedMatchCount {
            count: 0,
            reached_limit: true,
        };
    }
    let count = regex.find_iter(haystack).take(remaining).count();
    BoundedMatchCount {
        count,
        reached_limit: count == remaining,
    }
}

fn step_compressed_cursor(
    surfaces: &[FindSurface],
    cursor: FindCursor,
    total: usize,
    capped: bool,
    direction: FindDirection,
) -> Option<(FindCursor, bool)> {
    let current = surfaces.get(cursor.surface)?;
    if current.count == 0 || total == 0 {
        return None;
    }
    // The final retained occurrence is not necessarily the real final match
    // when scanning stopped at the cap. Do not wrap through VTE: Next would
    // select cap+1, while Previous from the first match would select the real
    // (unknown) tail and desynchronize the compressed cursor.
    if capped
        && (matches!(
            direction,
            FindDirection::Next if cursor.global + 1 == total
        ) || matches!(direction, FindDirection::Previous if cursor.global == 0))
    {
        return Some((cursor, false));
    }

    let mut next = cursor;
    let surface_changed = match direction {
        FindDirection::Next if cursor.occurrence + 1 < current.count => {
            next.occurrence += 1;
            false
        }
        FindDirection::Next => {
            next.surface = (cursor.surface + 1) % surfaces.len();
            next.occurrence = 0;
            true
        }
        FindDirection::Previous if cursor.occurrence > 0 => {
            next.occurrence -= 1;
            false
        }
        FindDirection::Previous => {
            next.surface = if cursor.surface == 0 {
                surfaces.len() - 1
            } else {
                cursor.surface - 1
            };
            next.occurrence = surfaces[next.surface].count.checked_sub(1)?;
            true
        }
    };
    next.global = match direction {
        FindDirection::Next => (cursor.global + 1) % total,
        FindDirection::Previous if cursor.global == 0 => total - 1,
        FindDirection::Previous => cursor.global - 1,
    };
    Some((next, surface_changed))
}

fn native_cursor_action(
    surface: &FindSurface,
    occurrence: usize,
    direction: FindDirection,
) -> Option<NativeCursorAction> {
    if occurrence >= surface.count {
        return None;
    }
    if surface.vte_cursor == Some(occurrence) {
        return Some(NativeCursorAction::AlreadySelected);
    }
    let wrap_once = match (surface.vte_cursor, direction) {
        (None, FindDirection::Next) if occurrence == 0 => false,
        (None, FindDirection::Previous) if occurrence + 1 == surface.count => false,
        (Some(current), FindDirection::Next)
            if current + 1 < surface.count && occurrence == current + 1 =>
        {
            false
        }
        (Some(current), FindDirection::Previous) if current > 0 && occurrence + 1 == current => {
            false
        }
        (Some(current), FindDirection::Next)
            if surface.complete && current + 1 == surface.count && occurrence == 0 =>
        {
            true
        }
        (Some(0), FindDirection::Previous)
            if surface.complete && occurrence + 1 == surface.count =>
        {
            true
        }
        _ => return None,
    };
    Some(NativeCursorAction::Step { wrap_once })
}

fn find_progress(state: &FindState) -> Option<FindProgress> {
    (!state.surfaces.is_empty() && state.total > 0).then_some(FindProgress {
        current: state.cursor.global + 1,
        total: state.total,
        capped: state.capped,
        scan_limited: state.scan_limited,
    })
}

/// One result row from the built-in cross-block substring/regex scan. Carries enough
/// context for a flat result list — block id (for jump), surface flag (so
/// the per-block VTE search cursor goes to the right widget), the 1-based
/// line number inside that surface, the line snippet itself (trimmed/
/// truncated for display), and a one-line cmd preview for context.
#[derive(Clone, Debug)]
pub struct CrossBlockHit {
    pub block_id: u64,
    pub is_output: bool,
    pub line_no: usize,
    pub line_text: String,
    pub cmd_preview: String,
}

/// Trim a line to a reasonable display width — the palette row is one
/// horizontal line so an unbounded long line (think bundled JSON) would
/// just blow out the dialog width. We truncate with a leading ellipsis if
/// the match isn't near the start, but for the MVP we just hard-cap.
fn snippet(line: &str) -> String {
    const CAP: usize = 240;
    let mut chars = line.chars();
    let mut snippet: String = chars.by_ref().take(CAP).collect();
    if chars.next().is_some() {
        snippet.push('…');
    }
    snippet
}

fn command_preview(command: &str) -> String {
    snippet(command.lines().next().unwrap_or(command))
}

fn duration_matches(duration: Option<u64>, filters: &BlockFilters) -> bool {
    let needs_duration =
        filters.min_duration_ms.is_some() || filters.max_duration_ms.is_some() || filters.slow_only;
    if !needs_duration {
        return true;
    }
    let Some(duration) = duration else {
        return false;
    };
    if filters.min_duration_ms.is_some_and(|min| duration < min) {
        return false;
    }
    if filters.max_duration_ms.is_some_and(|max| duration > max) {
        return false;
    }
    !filters.slow_only || duration >= filters.slow_threshold_ms
}

fn find_surface_block<'a>(
    finished: &'a [super::FinishedBlock],
    surface: &FindSurface,
) -> Option<&'a super::FinishedBlock> {
    finished
        .get(surface.block_index)
        .filter(|block| block.id == surface.block_id)
        .or_else(|| finished.iter().find(|block| block.id == surface.block_id))
}

fn find_highlight_block<'a>(
    finished: &'a [super::FinishedBlock],
    highlight: &FindHighlight,
) -> Option<&'a super::FinishedBlock> {
    finished
        .get(highlight.block_index)
        .filter(|block| block.id == highlight.block_id)
        .or_else(|| finished.iter().find(|block| block.id == highlight.block_id))
}

#[allow(dead_code)]
impl TermView {
    /// Search blocks for a query string (case-insensitive).
    /// Returns indices of matching blocks.
    pub fn search_blocks(&self, query: &str) -> Vec<usize> {
        self.search_blocks_with_filters(query, &BlockFilters::default())
    }

    /// Search blocks with optional filters
    pub fn search_blocks_with_filters(&self, query: &str, filters: &BlockFilters) -> Vec<usize> {
        let q = query.to_lowercase();
        let q_bytes = q.as_bytes();

        let re = if filters.use_regex && !query.is_empty() {
            regex::RegexBuilder::new(query)
                .case_insensitive(true)
                .build()
                .ok()
        } else {
            None
        };

        let results: Vec<usize> = self
            .block_data
            .borrow()
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                let text_match = if q.is_empty() {
                    true
                } else if let Some(ref re) = re {
                    re.is_match(&b.prompt) || re.is_match(&b.cmd) || re.is_match(&b.output)
                } else {
                    contains_case_insensitive(b.prompt.as_bytes(), q_bytes)
                        || contains_case_insensitive(b.cmd.as_bytes(), q_bytes)
                        || contains_case_insensitive(b.output.as_bytes(), q_bytes)
                };

                if !text_match {
                    return false;
                }

                // Both predicates use the completed, resolved-command outcome.
                // In particular, background output never matches a raw status
                // attached by a producer, while Unknown matches no exact code.
                if !outcome_matches_filters(&b.cmd, b.exit_code, filters) {
                    return false;
                }

                if !duration_matches(b.duration_ms, filters) {
                    return false;
                }

                true
            })
            .map(|(i, _)| i)
            .collect();

        results
    }

    /// Highlight occurrences of `query` across the finished blocks and focus
    /// the first hit. Match metadata is compressed to one count per VTE surface,
    /// and scanning stops as soon as [`FIND_MATCH_LIMIT`] is reached.
    pub(crate) fn find_in_blocks(&self, query: &str, use_regex: bool) -> FindSearchResult {
        self.clear_find();
        if query.is_empty() {
            return FindSearchResult::NoMatches;
        }
        let pattern = if use_regex {
            query.to_string()
        } else {
            regex::escape(query)
        };
        match regex_consumption(&pattern) {
            Ok(RegexConsumption::Consuming) => {}
            Ok(RegexConsumption::Never) => return FindSearchResult::NoMatches,
            Ok(RegexConsumption::ZeroWidth) | Err(_) => {
                return FindSearchResult::InvalidRegex;
            }
        }
        let re = match regex::RegexBuilder::new(&pattern)
            .case_insensitive(true)
            .multi_line(true)
            .build()
        {
            Ok(re) => re,
            Err(_) => return FindSearchResult::InvalidRegex,
        };

        // Compile the same pattern for VTE (PCRE2) so its native highlighter
        // paints every hit and its search cursor can step within each block.
        let vte_re = match vte4::Regex::for_search(&pattern, VTE_SEARCH_FLAGS) {
            Ok(r) => r,
            Err(_) => return FindSearchResult::InvalidRegex,
        };

        let mut surfaces = Vec::new();
        let mut total = 0usize;
        let mut match_limited = false;
        let mut scan_limited = false;
        let mut scan_budget = FindScanBudget::new();
        'finished: {
            let finished = self.finished_blocks.borrow();
            for (block_index, block) in finished.iter().enumerate() {
                // Count exactly the bounded command text fed to the command VTE,
                // not an original tail that the renderer deliberately omitted.
                let displayed_command = crate::review_input::safe_multiline_display(
                    &block.cmd_text,
                    crate::review_input::MAX_REVIEW_INPUT_BYTES,
                );
                let command_prefix = scan_budget.take_prefix(&displayed_command);
                let command = bounded_match_count(
                    &re,
                    command_prefix.text,
                    FIND_MATCH_LIMIT.saturating_sub(total),
                );
                if command.count > 0 {
                    block.command_vte.search_set_regex(Some(&vte_re), 0);
                    block.command_vte.search_set_wrap_around(false);
                    surfaces.push(FindSurface {
                        block_id: block.id,
                        block_index,
                        is_output: false,
                        is_live: false,
                        count: command.count,
                        vte_cursor: None,
                        complete: true,
                    });
                    total += command.count;
                }
                if command.reached_limit {
                    if command.count > 0 {
                        surfaces
                            .last_mut()
                            .expect("a matching command surface was just appended")
                            .complete = false;
                    }
                    match_limited = true;
                    break 'finished;
                }
                if command_prefix.incomplete || scan_budget.time_exhausted() {
                    if command_prefix.incomplete && command.count > 0 {
                        surfaces
                            .last_mut()
                            .expect("a matching command surface was just appended")
                            .complete = false;
                    }
                    scan_limited = true;
                    break 'finished;
                }

                // Enforce the aggregate budget before ANSI stripping. Calling
                // `with_stripped_output` here would allocate and permanently
                // cache a full plain-text copy for every visited block even when
                // only a small prefix is permitted.
                let (output, output_incomplete) = {
                    // Per-block filters re-feed only `displayed_output` into the
                    // VTE. Searching the hidden full capture would create logical
                    // matches which VTE can never focus.
                    let raw_output = block.displayed_output.borrow();
                    let output_prefix = scan_budget.take_prefix(&raw_output);
                    let plain_output = super::strip_ansi(output_prefix.text);
                    (
                        bounded_match_count(
                            &re,
                            &plain_output,
                            FIND_MATCH_LIMIT.saturating_sub(total),
                        ),
                        output_prefix.incomplete,
                    )
                };
                if output.count > 0 {
                    block.output_vte.search_set_regex(Some(&vte_re), 0);
                    block.output_vte.search_set_wrap_around(false);
                    surfaces.push(FindSurface {
                        block_id: block.id,
                        block_index,
                        is_output: true,
                        is_live: false,
                        count: output.count,
                        vte_cursor: None,
                        complete: true,
                    });
                    total += output.count;
                }
                if output.reached_limit {
                    if output.count > 0 {
                        surfaces
                            .last_mut()
                            .expect("a matching output surface was just appended")
                            .complete = false;
                    }
                    match_limited = true;
                    break 'finished;
                }
                if output_incomplete || scan_budget.time_exhausted() {
                    if output_incomplete && output.count > 0 {
                        surfaces
                            .last_mut()
                            .expect("a matching output surface was just appended")
                            .complete = false;
                    }
                    scan_limited = true;
                    break 'finished;
                }
            }
        }

        // The still-running command's output is searchable too (document
        // order: it sits below every finished block). Counted from the
        // accumulated raw capture, so only states that accumulate qualify;
        // VTE's own highlighter paints and steps the on-screen hits.
        if !match_limited
            && !scan_limited
            && matches!(
                self.bstate.get(),
                super::BlockState::CollectingOutput | super::BlockState::PostCommand
            )
        {
            let (live_raw, live_raw_incomplete) = self
                .active
                .borrow()
                .output_text_prefix(scan_budget.remaining_bytes());
            let live_prefix = scan_budget.take_prefix(&live_raw);
            let live_text = super::strip_ansi(live_prefix.text);
            let live = bounded_match_count(&re, &live_text, FIND_MATCH_LIMIT.saturating_sub(total));
            if live.count > 0 {
                self.active_vte.search_set_regex(Some(&vte_re), 0);
                self.active_vte.search_set_wrap_around(false);
                surfaces.push(FindSurface {
                    block_id: 0,
                    block_index: 0,
                    is_output: true,
                    is_live: true,
                    count: live.count,
                    vte_cursor: None,
                    complete: true,
                });
                total += live.count;
            }
            match_limited = live.reached_limit;
            scan_limited = !match_limited
                && (live_raw_incomplete || live_prefix.incomplete || scan_budget.time_exhausted());
            if live.count > 0 && (match_limited || live_raw_incomplete || live_prefix.incomplete) {
                surfaces
                    .last_mut()
                    .expect("a matching live surface was just appended")
                    .complete = false;
            }
        }

        if surfaces.is_empty() {
            return if scan_limited {
                FindSearchResult::ScanLimit
            } else {
                FindSearchResult::NoMatches
            };
        }
        let capped = match_limited || scan_limited;
        {
            let mut st = self.find_state.borrow_mut();
            st.surfaces = surfaces;
            st.cursor = FindCursor::default();
            st.total = total;
            st.capped = capped;
            st.scan_limited = scan_limited;
        }
        if !self.focus_current_match() {
            self.clear_find();
            return FindSearchResult::NoMatches;
        }
        self.scroll_to_current_match();
        FindSearchResult::Matches(FindProgress {
            current: 1,
            total,
            capped,
            scan_limited,
        })
    }

    /// Step to the next match. Exact result sets wrap; capped sets stop at the
    /// known edge rather than entering uncounted VTE matches.
    pub(crate) fn find_next(&self) -> FindNavigationResult {
        self.step_find(FindDirection::Next)
    }

    /// Step to the previous match. Exact result sets wrap; capped sets stop at
    /// the known edge rather than entering the unknown real tail.
    pub(crate) fn find_prev(&self) -> FindNavigationResult {
        self.step_find(FindDirection::Previous)
    }

    fn step_find(&self, direction: FindDirection) -> FindNavigationResult {
        let (current, next, current_progress) = {
            let state = self.find_state.borrow();
            let Some(current_progress) = find_progress(&state) else {
                return FindNavigationResult::Inactive;
            };
            let step = step_compressed_cursor(
                &state.surfaces,
                state.cursor,
                state.total,
                state.capped,
                direction,
            );
            let current = state.cursor;
            drop(state);
            let Some((next, _surface_changed)) = step else {
                self.clear_find();
                return FindNavigationResult::Invalidated;
            };
            (current, next, current_progress)
        };
        if next == current {
            return FindNavigationResult::Progress(current_progress);
        }

        if !self.focus_surface_occurrence(next.surface, next.occurrence, direction) {
            self.clear_find();
            return FindNavigationResult::Invalidated;
        }
        {
            let mut state = self.find_state.borrow_mut();
            if state.cursor != current {
                drop(state);
                self.clear_find();
                return FindNavigationResult::Invalidated;
            }
            state.cursor = next;
        }
        self.scroll_to_current_match();
        let progress = {
            let state = self.find_state.borrow();
            find_progress(&state)
        };
        let Some(progress) = progress else {
            self.clear_find();
            return FindNavigationResult::Invalidated;
        };
        FindNavigationResult::Progress(progress)
    }

    /// Ask VTE to select one exact compressed occurrence. The native and logical
    /// cursors advance together only after VTE confirms success. Native wrapping
    /// is enabled for one call only when the entire target surface was scanned;
    /// it is always left disabled, especially for capped prefixes.
    fn focus_surface_occurrence(
        &self,
        surface_index: usize,
        occurrence: usize,
        direction: FindDirection,
    ) -> bool {
        let surface = {
            let state = self.find_state.borrow();
            let Some(surface) = state.surfaces.get(surface_index) else {
                return false;
            };
            surface.clone()
        };
        let wrap_once = match native_cursor_action(&surface, occurrence, direction) {
            Some(NativeCursorAction::AlreadySelected) => return true,
            Some(NativeCursorAction::Step { wrap_once }) => wrap_once,
            None => return false,
        };

        let vte = if surface.is_live {
            self.active_vte.clone()
        } else {
            let finished = self.finished_blocks.borrow();
            let Some(block) = find_surface_block(&finished, &surface) else {
                return false;
            };
            if surface.is_output {
                block.output_vte.clone()
            } else {
                block.command_vte.clone()
            }
        };
        vte.search_set_wrap_around(wrap_once);
        let found = match direction {
            FindDirection::Next => vte.search_find_next(),
            FindDirection::Previous => vte.search_find_previous(),
        };
        vte.search_set_wrap_around(false);
        if !found {
            return false;
        }

        let mut state = self.find_state.borrow_mut();
        let Some(target) = state.surfaces.get_mut(surface_index) else {
            return false;
        };
        if target.block_id != surface.block_id
            || target.is_output != surface.is_output
            || target.is_live != surface.is_live
            || target.count != surface.count
        {
            return false;
        }
        target.vte_cursor = Some(occurrence);
        true
    }

    /// Move VTE's search cursor to the very first match of the current pass.
    fn focus_current_match(&self) -> bool {
        self.focus_surface_occurrence(0, 0, FindDirection::Next)
    }

    fn scroll_to_current_match(&self) {
        let finished = self.finished_blocks.borrow();
        let st = self.find_state.borrow();
        let Some(surface) = st.surfaces.get(st.cursor.surface) else {
            return;
        };
        let widget = if surface.is_live {
            self.active.borrow().widget().clone()
        } else if let Some(block) = find_surface_block(&finished, surface) {
            block.widget().clone()
        } else {
            return;
        };
        let scroll = self.block_scroll.clone();
        glib::idle_add_local_once(move || {
            if let Some(point) =
                widget.compute_point(&scroll, &gtk4::graphene::Point::new(0.0, 0.0))
            {
                let adj = scroll.vadjustment();
                let max_value = (adj.upper() - adj.page_size()).max(adj.lower());
                let target = adj.value() + point.y() as f64 - adj.page_size() / 3.0;
                adj.set_value(target.clamp(adj.lower(), max_value));
            }
        });
    }

    /// Cross-block substring/regex flat-result scan over cached stripped output
    /// and command text. Caller passes a literal substring (case-insensitive)
    /// when `is_regex == false`, else a regex.
    ///
    /// Returns at most `max_hits` hits in block-list order; each hit carries
    /// enough context (line number + the raw line + cmd preview) to drive a
    /// palette UI that lets the user pick one and jump to it.
    ///
    /// Errors only on invalid regex; an empty pattern returns `Ok(vec![])`
    /// so the caller can clear results without a special branch.
    pub fn cross_block_search(
        &self,
        pattern: &str,
        is_regex: bool,
        max_hits: usize,
    ) -> Result<Vec<CrossBlockHit>, String> {
        if pattern.is_empty() {
            return Ok(Vec::new());
        }
        let compiled_pattern = if is_regex {
            pattern.to_string()
        } else {
            regex::escape(pattern)
        };
        let re = regex::RegexBuilder::new(&compiled_pattern)
            .case_insensitive(true)
            .multi_line(true)
            .build()
            .map_err(|e| format!("{e}"))?;

        let finished = self.finished_blocks.borrow();
        let mut hits: Vec<CrossBlockHit> = Vec::new();

        for block in finished.iter() {
            if hits.len() >= max_hits {
                break;
            }
            let cmd_preview = command_preview(&block.cmd_text);

            // Cmd surface — usually 1 line, but multiline commands exist.
            for (ln_idx, line) in block.cmd_text.lines().enumerate() {
                if hits.len() >= max_hits {
                    break;
                }
                if re.is_match(line) {
                    hits.push(CrossBlockHit {
                        block_id: block.id,
                        is_output: false,
                        line_no: ln_idx + 1,
                        line_text: snippet(line),
                        cmd_preview: cmd_preview.clone(),
                    });
                }
            }

            // Output surface — uses the cached ANSI-stripped view.
            block.with_stripped_output(|s| {
                for (ln_idx, line) in s.lines().enumerate() {
                    if hits.len() >= max_hits {
                        break;
                    }
                    if re.is_match(line) {
                        hits.push(CrossBlockHit {
                            block_id: block.id,
                            is_output: true,
                            line_no: ln_idx + 1,
                            line_text: snippet(line),
                            cmd_preview: cmd_preview.clone(),
                        });
                    }
                }
            });
        }
        Ok(hits)
    }

    /// Scroll the named block into view (by stable id, not list index).
    /// Returns `false` if the id is unknown — likely evicted by the
    /// `max_blocks` cap or deleted via the per-block menu.
    pub fn scroll_to_block_id(&self, block_id: u64) -> bool {
        let finished = self.finished_blocks.borrow();
        let Some(block) = finished.iter().find(|b| b.id == block_id) else {
            return false;
        };
        self.cross_selection.clear_all();
        replace_finished_block_selection(
            &finished,
            &self.selected_block_ids,
            &self.selected_block_id,
            &self.selection_anchor_id,
            Some(block_id),
        );
        block.widget().grab_focus();
        let adj = self.block_scroll.vadjustment();
        if let Some(value) = block
            .widget()
            .compute_point(&self.block_scroll, &gtk4::graphene::Point::new(0.0, 0.0))
        {
            let max_value = (adj.upper() - adj.page_size()).max(adj.lower());
            let target = adj.value() + value.y() as f64;
            adj.set_value(target.clamp(adj.lower(), max_value));
        }
        true
    }

    /// Light up the chosen block's command/output VTE with a PCRE2 search
    /// for `pattern` and advance its internal search cursor to the first
    /// hit. Other blocks keep whatever highlight state they had — this is
    /// the "jump to this hit" companion for `cross_block_search`. Returns
    /// `false` when the id is unknown or the pattern can't compile.
    pub fn focus_match_in_block(
        &self,
        block_id: u64,
        pattern: &str,
        is_regex: bool,
        is_output: bool,
    ) -> bool {
        if pattern.is_empty() {
            return false;
        }
        let compiled = if is_regex {
            pattern.to_string()
        } else {
            regex::escape(pattern)
        };
        let Ok(vte_re) = vte4::Regex::for_search(&compiled, VTE_SEARCH_FLAGS) else {
            return false;
        };
        let (block_index, vte) = {
            let finished = self.finished_blocks.borrow();
            let Some((block_index, block)) = finished
                .iter()
                .enumerate()
                .find(|(_, block)| block.id == block_id)
            else {
                return false;
            };
            let vte = if is_output {
                block.output_vte.clone()
            } else {
                block.command_vte.clone()
            };
            (block_index, vte)
        };
        vte.search_set_regex(Some(&vte_re), 0);
        vte.search_set_wrap_around(true);
        if !vte.search_find_next() {
            vte.search_set_regex(None::<&vte4::Regex>, 0);
            return false;
        }
        let highlight = FindHighlight {
            block_id,
            block_index,
            is_output,
        };
        let mut state = self.find_state.borrow_mut();
        if !state.extra_highlights.contains(&highlight) {
            state.extra_highlights.push(highlight);
        }
        true
    }

    /// Remove all find highlights and reset the find cursor (call on close).
    pub fn clear_find(&self) {
        clear_find_state(
            self.find_state.as_ref(),
            self.finished_blocks.as_ref(),
            &self.active_vte,
        );
    }

    /// Get only failed blocks (exit_code != 0)
    pub fn get_failed_blocks(&self) -> Vec<usize> {
        let filters = BlockFilters {
            failed_only: true,
            ..Default::default()
        };
        self.search_blocks_with_filters("", &filters)
    }

    /// Get only slow blocks (duration > threshold)
    pub fn get_slow_blocks(&self, threshold_ms: u64) -> Vec<usize> {
        let filters = BlockFilters {
            slow_only: true,
            slow_threshold_ms: threshold_ms,
            ..Default::default()
        };
        self.search_blocks_with_filters("", &filters)
    }
}

/// Reset a pane's search before its finished-block structure changes. Resolve
/// the highlighted terminals while the block list is borrowed, then release
/// that borrow before calling into GTK so a synchronous signal cannot re-enter
/// a structural path and panic on the `RefCell`.
pub(super) fn clear_find_state(
    find_state: &std::cell::RefCell<FindState>,
    finished_blocks: &std::cell::RefCell<Vec<super::FinishedBlock>>,
    active_vte: &vte4::Terminal,
) {
    let (surfaces, extra_highlights) = {
        let state = std::mem::take(&mut *find_state.borrow_mut());
        (state.surfaces, state.extra_highlights)
    };
    let highlighted_vtes: Vec<vte4::Terminal> = {
        let finished = finished_blocks.borrow();
        surfaces
            .iter()
            .filter(|surface| !surface.is_live)
            .map(FindHighlight::from)
            .chain(extra_highlights.iter().copied())
            .filter_map(|highlight| {
                find_highlight_block(&finished, &highlight).map(|block| {
                    if highlight.is_output {
                        block.output_vte.clone()
                    } else {
                        block.command_vte.clone()
                    }
                })
            })
            .collect()
    };
    for vte in highlighted_vtes {
        vte.search_set_regex(None::<&vte4::Regex>, 0);
    }
    active_vte.search_set_regex(None::<&vte4::Regex>, 0);
}

#[cfg(test)]
mod tests {
    use super::{
        bounded_match_count, command_preview, duration_matches, native_cursor_action,
        outcome_matches_filters, regex_consumption, snippet, step_compressed_cursor, utf8_prefix,
        FindCursor, FindDirection, FindScanBudget, FindSurface, NativeCursorAction,
        RegexConsumption, VTE_SEARCH_FLAGS,
    };
    use crate::block_view::BlockFilters;
    use std::time::Instant;

    fn surface(count: usize, complete: bool) -> FindSurface {
        FindSurface {
            block_id: 1,
            block_index: 0,
            is_output: false,
            is_live: false,
            count,
            vte_cursor: None,
            complete,
        }
    }

    fn step(
        surfaces: &[FindSurface],
        cursor: FindCursor,
        total: usize,
        capped: bool,
        direction: FindDirection,
    ) -> FindCursor {
        step_compressed_cursor(surfaces, cursor, total, capped, direction)
            .expect("valid compressed cursor")
            .0
    }

    #[test]
    fn bounded_counter_stops_at_the_match_limit() {
        let regex = regex::Regex::new(".").unwrap();
        let counted = bounded_match_count(&regex, &"x".repeat(20_000), 10_000);
        assert_eq!(counted.count, 10_000);
        assert!(counted.reached_limit);
    }

    #[test]
    fn vte_search_uses_unicode_properties_like_the_rust_counter() {
        assert_ne!(VTE_SEARCH_FLAGS & pcre2_sys::PCRE2_UTF, 0);
        assert_ne!(VTE_SEARCH_FLAGS & pcre2_sys::PCRE2_UCP, 0);

        // Arabic-Indic digits are the common regression: Rust's default `\d`
        // counts them, while bare PCRE2 shorthand classes are ASCII-only.
        assert!(regex::Regex::new(r"\d").unwrap().is_match("١"));
        assert!(vte4::Regex::for_search(r"\d", VTE_SEARCH_FLAGS).is_ok());
    }

    #[test]
    fn zero_width_regexes_are_rejected_before_vte_and_consuming_anchors_are_allowed() {
        for pattern in [r"^", r"$", r"\b", r"a*", r"(?:x)?"] {
            assert_eq!(
                regex_consumption(pattern).unwrap(),
                RegexConsumption::ZeroWidth,
                "{pattern}"
            );
        }
        assert_eq!(
            regex_consumption(r"^foo").unwrap(),
            RegexConsumption::Consuming
        );
    }

    #[test]
    fn utf8_scan_prefix_never_splits_a_code_point() {
        assert_eq!(utf8_prefix("ab界cd", 4), "ab");
        assert_eq!(utf8_prefix("ab界cd", 5), "ab界");
        assert_eq!(utf8_prefix("ab界cd", usize::MAX), "ab界cd");
    }

    #[test]
    fn aggregate_scan_budget_reports_an_incomplete_utf8_safe_prefix() {
        let mut budget = FindScanBudget {
            remaining_bytes: 5,
            started: Instant::now(),
        };
        let first = budget.take_prefix("abc");
        assert_eq!(first.text, "abc");
        assert!(!first.incomplete);

        let second = budget.take_prefix("界z");
        assert_eq!(second.text, "");
        assert!(second.incomplete);
        assert_eq!(budget.remaining_bytes(), 2);
    }

    #[test]
    fn native_cursor_plan_tracks_boundaries_without_resetting_regex() {
        let mut complete = surface(3, true);
        assert_eq!(
            native_cursor_action(&complete, 0, FindDirection::Next),
            Some(NativeCursorAction::Step { wrap_once: false })
        );
        complete.vte_cursor = Some(0);
        assert_eq!(
            native_cursor_action(&complete, 0, FindDirection::Previous),
            Some(NativeCursorAction::AlreadySelected)
        );
        assert_eq!(
            native_cursor_action(&complete, 2, FindDirection::Previous),
            Some(NativeCursorAction::Step { wrap_once: true })
        );

        let mut incomplete = surface(3, false);
        incomplete.vte_cursor = Some(2);
        assert_eq!(
            native_cursor_action(&incomplete, 0, FindDirection::Next),
            None
        );
    }

    #[test]
    fn compressed_navigation_preserves_surface_order_and_direction_reversal() {
        let surfaces = [surface(2, true), surface(3, true)];
        let mut cursor = FindCursor::default();

        cursor = step(&surfaces, cursor, 5, false, FindDirection::Next);
        assert_eq!(
            (cursor.surface, cursor.occurrence, cursor.global),
            (0, 1, 1)
        );
        cursor = step(&surfaces, cursor, 5, false, FindDirection::Next);
        assert_eq!(
            (cursor.surface, cursor.occurrence, cursor.global),
            (1, 0, 2)
        );
        cursor = step(&surfaces, cursor, 5, false, FindDirection::Previous);
        assert_eq!(
            (cursor.surface, cursor.occurrence, cursor.global),
            (0, 1, 1)
        );
        cursor = step(&surfaces, cursor, 5, false, FindDirection::Next);
        assert_eq!(
            (cursor.surface, cursor.occurrence, cursor.global),
            (1, 0, 2)
        );
    }

    #[test]
    fn exact_navigation_wraps_but_capped_navigation_stops_at_both_edges() {
        let exact = [surface(2, true)];
        let last = FindCursor {
            surface: 0,
            occurrence: 1,
            global: 1,
        };
        assert_eq!(
            step(&exact, last, 2, false, FindDirection::Next),
            FindCursor::default()
        );
        assert_eq!(
            step(
                &exact,
                FindCursor::default(),
                2,
                false,
                FindDirection::Previous,
            ),
            last
        );

        let capped = [surface(2, true), surface(2, false)];
        let capped_last = FindCursor {
            surface: 1,
            occurrence: 1,
            global: 3,
        };
        assert_eq!(
            step(&capped, capped_last, 4, true, FindDirection::Next),
            capped_last
        );
        assert_eq!(
            step(
                &capped,
                FindCursor::default(),
                4,
                true,
                FindDirection::Previous,
            ),
            FindCursor::default()
        );
    }

    #[test]
    fn unknown_duration_does_not_match_duration_filters() {
        let filters = BlockFilters {
            slow_only: true,
            slow_threshold_ms: 1_000,
            ..Default::default()
        };
        assert!(!duration_matches(None, &filters));
    }

    #[test]
    fn duration_boundaries_are_inclusive() {
        let filters = BlockFilters {
            min_duration_ms: Some(500),
            max_duration_ms: Some(1_500),
            ..Default::default()
        };
        assert!(duration_matches(Some(500), &filters));
        assert!(duration_matches(Some(1_500), &filters));
        assert!(!duration_matches(Some(499), &filters));
        assert!(!duration_matches(Some(1_501), &filters));
    }

    #[test]
    fn duration_is_irrelevant_without_duration_predicates() {
        assert!(duration_matches(None, &BlockFilters::default()));
    }

    #[test]
    fn outcome_filters_ignore_raw_status_on_background_output() {
        let exact = BlockFilters {
            exit_code: Some(7),
            ..Default::default()
        };
        let failed = BlockFilters {
            failed_only: true,
            ..Default::default()
        };

        assert!(!outcome_matches_filters("", Some(7), &exact));
        assert!(!outcome_matches_filters("\t ", Some(7), &failed));
        assert!(outcome_matches_filters("false", Some(7), &exact));
        assert!(outcome_matches_filters("false", Some(7), &failed));
    }

    #[test]
    fn command_without_a_reported_status_matches_neither_exit_filter() {
        let exact_success = BlockFilters {
            exit_code: Some(0),
            ..Default::default()
        };
        let failed = BlockFilters {
            failed_only: true,
            ..Default::default()
        };

        assert!(!outcome_matches_filters("cargo test", None, &exact_success));
        assert!(!outcome_matches_filters("cargo test", None, &failed));
    }

    #[test]
    fn snippet_passes_through_short_line() {
        assert_eq!(snippet("hello world"), "hello world");
    }

    #[test]
    fn snippet_truncates_long_line_with_ellipsis() {
        let long: String = "a".repeat(500);
        let out = snippet(&long);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().filter(|&c| c == 'a').count(), 240);
    }

    #[test]
    fn snippet_truncates_cjk_and_emoji_on_char_boundaries() {
        for line in [
            format!("a{}", "界".repeat(240)),
            format!("a{}", "🙂".repeat(240)),
        ] {
            let out = snippet(&line);
            assert!(out.ends_with('…'));
            assert_eq!(out.chars().count(), 241);
            assert_eq!(
                out.chars().take(240).collect::<String>(),
                line.chars().take(240).collect::<String>()
            );
        }
    }

    #[test]
    fn command_preview_bounds_long_first_line_before_hits_clone_it() {
        let command = format!("{}\nignored second line", "x".repeat(256 * 1024));
        let preview = command_preview(&command);

        assert_eq!(preview.chars().count(), 241);
        assert!(preview.ends_with('…'));
        assert!(!preview.contains("ignored second line"));
    }
}
