use gtk4::gdk::RGBA;
use gtk4::pango::FontDescription;
use gtk4::prelude::*;
use gtk4::{glib, Orientation, ScrolledWindow};
use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::io;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;
use vte4::Terminal;
use vte4::TerminalExt;

use crate::config::Config;
use crate::parser::{
    ColorKind, CommandMeta, KeyboardProtocolQuery, Parser, ParserConfig, ParserEvent,
};
use crate::pty::OwnedPty;
use crate::pty_input::{self, Paste, PasteModes, PastePolicy, UnbracketedMultiline};
use crate::terminal::{apply_terminal_theme, focus_terminal};
use bounded_bytes::BoundedByteRing;

mod alt_screen;
mod ansi;
mod blocks;
mod bounded_bytes;
mod cross_selection;
mod css;
mod export;
mod find;
mod history;
mod kitty_graphics;
#[allow(dead_code)]
mod palette;
mod scroll;
mod selection_hold;
pub(crate) use alt_screen::*;
pub(crate) use ansi::*;
pub(crate) use blocks::*;
pub(crate) use cross_selection::*;
pub(crate) use css::*;
pub(crate) use export::SessionExportFormat;
pub(crate) use find::*;
#[allow(unused_imports)]
pub(crate) use palette::*;
pub(crate) use scroll::*;
pub(crate) use selection_hold::*;

// ── perf profiling (env JTERM_PROF=1) ───────────────────────────────────────
pub(crate) fn prof_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("JTERM_PROF").is_ok())
}

// Global block ID counter
static BLOCK_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Why a review-gated command can or cannot be written to the live Block
/// prompt. Keeping this richer than a boolean lets every AI surface explain
/// the exact recovery step without weakening the empty, idle-prompt boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandPromptStatus {
    Ready,
    HasInput,
    Running,
    Fullscreen,
    Initializing,
    ShellIntegrationUnavailable,
}

impl CommandPromptStatus {
    pub(crate) fn is_ready(self) -> bool {
        self == Self::Ready
    }

    pub(crate) fn short_label(self) -> &'static str {
        match self {
            Self::Ready => "Prompt ready",
            Self::HasInput => "Prompt has input",
            Self::Running => "Command running",
            Self::Fullscreen => "Full-screen app active",
            Self::Initializing => "Prompt initializing",
            Self::ShellIntegrationUnavailable => "Shell integration required",
        }
    }

    pub(crate) fn blocked_message(self) -> &'static str {
        match self {
            Self::Ready => "The pinned Block prompt is ready.",
            Self::HasInput => {
                "The pinned shell prompt already contains input. Clear it and press Enter to reach a fresh prompt, then try again."
            }
            Self::Running => {
                "A command is still running in the pinned Block pane. Wait for it to finish and for a fresh prompt, then try again."
            }
            Self::Fullscreen => {
                "A full-screen terminal application owns the pinned pane. Exit it before inserting or approving a command."
            }
            Self::Initializing => {
                "The pinned Block prompt is still initializing. Wait for the shell prompt, then try again."
            }
            Self::ShellIntegrationUnavailable => {
                "Shell integration is not active, so jterm4 cannot safely verify an idle prompt. Load the jterm4 shell integration and open a new shell."
            }
        }
    }
}

fn next_block_id() -> u64 {
    BLOCK_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Keep newly-created block ids above every id restored from persistent history.
fn reserve_block_ids_after(blocks: &VecDeque<BlockData>, counter: &AtomicU64) {
    if let Some(max_id) = blocks.iter().map(|block| block.id).max() {
        counter.fetch_max(max_id.saturating_add(1), Ordering::Relaxed);
    }
}

/// Repair duplicate ids left by older sessions, then reserve the allocator above
/// all restored ids. Selection, delete, export, search, and bookmarks are id-keyed;
/// allowing a restarted process to issue id 0 again makes those operations target
/// the wrong card.
fn normalize_loaded_block_ids(blocks: &mut VecDeque<BlockData>, counter: &AtomicU64) -> usize {
    reserve_block_ids_after(blocks, counter);

    let mut seen = HashSet::with_capacity(blocks.len());
    let mut repaired = 0usize;
    for block in blocks {
        if seen.insert(block.id) {
            continue;
        }

        let replacement = loop {
            let candidate = counter.fetch_add(1, Ordering::Relaxed);
            if seen.insert(candidate) {
                break candidate;
            }
        };
        block.id = replacement;
        repaired += 1;
    }
    repaired
}

/// Update the jump-to-bottom FAB's label to show an unread-block badge: just the
/// chevron when nothing is pending, chevron + count (clamped to "99+") otherwise.
fn set_jump_fab_label(fab: &gtk4::Button, unread: u32) {
    if unread > 0 {
        let n = if unread > 99 {
            "99+".to_string()
        } else {
            unread.to_string()
        };
        fab.set_label(&format!("\u{f078}  {}", n));
    } else {
        fab.set_label("\u{f078}");
    }
}

/// Probe the cwd for git metadata and update the strip label. Hides the
/// label when cwd is empty, missing, or not inside a repo — the user
/// shouldn't see a stale branch from a previous pane state.
fn refresh_repo_strip(label: &gtk4::Label, cwd: &str) {
    if cwd.is_empty() {
        label.set_visible(false);
        return;
    }
    let path = std::path::Path::new(cwd);
    match crate::git_meta::read(path) {
        Some(meta) => {
            label.set_text(&crate::git_meta::format_strip(&meta));
            label.set_visible(true);
        }
        None => {
            label.set_visible(false);
        }
    }
}

fn sample_output_for_event(output: &str) -> String {
    const MAX_CHARS: usize = 32 * 1024;
    if output.len() <= MAX_CHARS {
        return output.to_string();
    }
    let half = MAX_CHARS / 2;
    let head_end = output
        .char_indices()
        .map(|(i, _)| i)
        .take_while(|&i| i <= half)
        .last()
        .unwrap_or(0);
    let tail_start = output
        .char_indices()
        .map(|(i, _)| i)
        .find(|&i| i >= output.len().saturating_sub(half))
        .unwrap_or(output.len());
    format!(
        "{}\n... [{} bytes elided] ...\n{}",
        &output[..head_end],
        tail_start.saturating_sub(head_end),
        &output[tail_start..]
    )
}

/// Shell integration normally places PromptEnd after the prompt, so the VTE
/// range starts at the first command cell. Some prompt integrations emit the
/// marker early; in that case the captured range includes the rendered prompt.
/// Finished blocks already represent the prompt with their own chevron/header,
/// so remove only an exact leading prompt to avoid duplicated, drifting command
/// rows such as `❯ yj ~ ❯ pwd`.
fn normalize_captured_command(captured: &str, prompt: &str) -> String {
    let captured = captured.trim();
    let prompt = prompt.trim();
    if !prompt.is_empty() {
        if let Some(command) = captured.strip_prefix(prompt) {
            return command.trim_start().to_string();
        }
    }
    captured.to_string()
}

/// Resolve the command at CommandStart without trusting VTE feed timing. The PTY
/// reader can deliver the echoed command and OSC 133;C in one chunk: `feed()`
/// queues the echo for VTE, then the semantic event is handled immediately, so
/// the text range can still be empty. An explicitly submitted programmatic
/// command is authoritative because it can race ahead of VTE rendering. For
/// interactive input, the keystroke shadow is deliberately only a fallback; a
/// settled VTE capture remains authoritative for history recall, autosuggestions,
/// IME, and shell line-editor redraws.
fn resolve_submitted_command(
    captured: &str,
    prompt: &str,
    typed_shadow: &str,
    external_submission: Option<&str>,
) -> String {
    if let Some(command) = external_submission {
        return bounded_command_text(command.trim());
    }
    let captured = normalize_captured_command(captured, prompt);
    if captured.trim().is_empty() {
        bounded_command_text(typed_shadow.trim())
    } else {
        bounded_command_text(&captured)
    }
}

/// Stand-in command line for a shell that told us it *had* a command but could
/// not fit it in its OSC 133 packet, and whose echo the VTE read missed. Kept
/// distinct from the "(command capture unavailable)" placeholder: this one is a
/// bounded-packet outcome, not a capture race.
const TRUNCATED_COMMAND_PLACEHOLDER: &str = "(command too long for shell integration)";
const MAX_COMMAND_CAPTURE_BYTES: usize = crate::review_input::MAX_REVIEW_INPUT_BYTES;
const MAX_TYPED_COMMAND_SHADOW_BYTES: usize = MAX_COMMAND_CAPTURE_BYTES;
const MAX_PROMPT_CAPTURE_BYTES: usize = 64 * 1024;
const MAX_SELECTED_CLIPBOARD_BYTES: usize = 32 * 1024 * 1024;

fn bounded_command_text(command: &str) -> String {
    if command.len() > MAX_COMMAND_CAPTURE_BYTES {
        TRUNCATED_COMMAND_PLACEHOLDER.to_string()
    } else {
        command.to_string()
    }
}

fn append_bounded_text_tail(buffer: &mut String, text: &str, max_bytes: usize) {
    if max_bytes == 0 {
        buffer.clear();
        return;
    }
    if text.len() >= max_bytes {
        let mut start = text.len() - max_bytes;
        while !text.is_char_boundary(start) {
            start += 1;
        }
        buffer.clear();
        buffer.push_str(&text[start..]);
        return;
    }
    let overflow = buffer
        .len()
        .checked_add(text.len())
        .map(|length| length.saturating_sub(max_bytes))
        .unwrap_or(buffer.len());
    if overflow != 0 {
        let mut start = overflow.min(buffer.len());
        while !buffer.is_char_boundary(start) {
            start += 1;
        }
        buffer.drain(..start);
    }
    buffer.push_str(text);
}

fn append_typed_command_shadow(buffer: &mut String, text: &str) {
    if buffer == TRUNCATED_COMMAND_PLACEHOLDER || text.is_empty() {
        return;
    }
    if buffer
        .len()
        .checked_add(text.len())
        .is_some_and(|length| length <= MAX_TYPED_COMMAND_SHADOW_BYTES)
    {
        buffer.push_str(text);
    } else {
        buffer.clear();
        buffer.push_str(TRUNCATED_COMMAND_PLACEHOLDER);
    }
}

fn pop_typed_command_shadow(buffer: &mut String) {
    if buffer != TRUNCATED_COMMAND_PLACEHOLDER {
        buffer.pop();
    }
}

#[derive(Debug)]
enum TypedShadowRollback {
    Unchanged,
    Truncate(usize),
    Restore(String),
}

impl TypedShadowRollback {
    fn apply(self, buffer: &mut String) {
        match self {
            Self::Unchanged => {}
            Self::Truncate(length) => buffer.truncate(length),
            Self::Restore(previous) => *buffer = previous,
        }
    }
}

/// Capture the cheapest exact rollback for a VTE commit. Ordinary typing only
/// appends, so retaining the previous byte length avoids cloning a potentially
/// long command on every keystroke. Destructive edits are rare and keep a full
/// snapshot.
fn vte_commit_shadow_rollback(buffer: &str, text: &str) -> TypedShadowRollback {
    if text.chars().all(|ch| !ch.is_control())
        && buffer != TRUNCATED_COMMAND_PLACEHOLDER
        && buffer
            .len()
            .checked_add(text.len())
            .is_some_and(|length| length <= MAX_TYPED_COMMAND_SHADOW_BYTES)
    {
        return TypedShadowRollback::Truncate(buffer.len());
    }
    if text
        .chars()
        .all(|ch| ch != '\x7f' && ch != '\x08' && ch.is_control())
        || buffer == TRUNCATED_COMMAND_PLACEHOLDER
    {
        return TypedShadowRollback::Unchanged;
    }
    TypedShadowRollback::Restore(buffer.to_string())
}

fn apply_vte_commit_to_shadow(buffer: &mut String, text: &str) {
    for ch in text.chars() {
        if matches!(ch, '\r' | '\n') {
            // Submitted — PromptEnd clears the shadow for the next prompt.
        } else if matches!(ch, '\x7f' | '\x08') {
            pop_typed_command_shadow(buffer);
        } else if ch.is_control() {
            // Other terminal control bytes do not belong in command text.
        } else {
            let mut encoded = [0_u8; 4];
            append_typed_command_shadow(buffer, ch.encode_utf8(&mut encoded));
        }
    }
}

fn command_capture_range_is_bounded(start_row: i64, end_row: i64, columns: i64) -> bool {
    end_row
        .checked_sub(start_row)
        .and_then(|rows| rows.checked_add(1))
        .filter(|rows| *rows > 0)
        .and_then(|rows| rows.checked_mul(columns.max(1)))
        .and_then(|cells| usize::try_from(cells).ok())
        .is_some_and(|cells| cells <= MAX_COMMAND_CAPTURE_BYTES)
}

/// The shell metadata carried from a command's OSC 133 `C`/`D` marks to the
/// block that is finalized at the next `PromptStart`.
///
/// jsh attaches its execution id, the command line it parsed, the cwd and the
/// duration it measured. All of it beats what this app can reconstruct: the id
/// is the only way to correlate captured output with a journal record, and the
/// duration is measured by the process that ran the command rather than by a
/// timer started when the frontend noticed the mark.
#[derive(Default)]
pub(crate) struct PendingCommandMeta {
    id: Option<String>,
    cwd: Option<String>,
    duration_ms: Option<u64>,
}

impl PendingCommandMeta {
    fn from_command_start(meta: &CommandMeta) -> Self {
        Self {
            id: meta
                .id
                .as_deref()
                .filter(|id| crate::review_input::valid_jsh_id(id))
                .map(str::to_owned),
            cwd: safe_command_metadata_cwd(meta.cwd.as_deref()),
            duration_ms: meta.duration_ms,
        }
    }

    /// Fold in the `D` packet. A shell may attach a field to only one of the two
    /// marks (jsh sends the duration only on `D`), so a value already in hand is
    /// never replaced by an absent one.
    ///
    /// `cwd` is deliberately fill-only: jsh's `D` packet carries `cwd_after`
    /// (`jsh/src/shell.rs`), the directory the shell is in *now*. Letting it win
    /// would label a `cd /tmp` block with `/tmp` instead of the directory the
    /// command actually ran in.
    fn merge_command_end(&mut self, meta: &CommandMeta) {
        if let Some(id) = meta
            .id
            .as_deref()
            .filter(|id| crate::review_input::valid_jsh_id(id))
        {
            self.id = Some(id.to_owned());
        }
        if self.cwd.is_none() {
            self.cwd = safe_command_metadata_cwd(meta.cwd.as_deref());
        }
        if meta.duration_ms.is_some() {
            self.duration_ms = meta.duration_ms;
        }
    }
}

fn safe_command_metadata_cwd(cwd: Option<&str>) -> Option<String> {
    cwd.filter(|cwd| {
        !cwd.is_empty()
            && cwd.len() <= 16 * 1024
            && !cwd.chars().any(char::is_control)
            && !crate::review_input::contains_visual_spoof(cwd)
    })
    .map(str::to_owned)
}

/// Desktop notification for a long command.
///
/// The shared notifier takes a concrete exit code and turns it into a ✓/✗ title,
/// which cannot express "the shell did not say". A status we never learned gets a
/// plain app notification instead of borrowing either verdict.
fn notify_long_block(command: &str, exit_code: Option<i32>, duration_ms: u64) {
    let command =
        crate::review_input::safe_inline_display(command.lines().next().unwrap_or(command), 1_024);
    match exit_code {
        Some(code) => crate::notify::long_block_finished(&command, code, duration_ms),
        None => crate::notify::app_notification(
            Some(&format!("? {command}")),
            &format!("Exit status unknown after {duration_ms} ms"),
        ),
    }
}

/// Largest captured output submitted to jsh's execution journal.
///
/// The journal's reader drops an output event whose text exceeds its own limit,
/// so an unbounded submission would be written to disk and then ignored — worse
/// than a bounded one that is actually readable.
const MAX_JOURNAL_OUTPUT_BYTES: usize = 256 * 1024;

/// Attach this pane's captured output to jsh's execution record for `id`.
///
/// Fire-and-forget: the journal queues on a writer thread and rejects the newest
/// item when saturated, so a stalled state directory can never block the GTK
/// main loop. jsh still owns the command, cwd, exit status and duration events;
/// only the rendered text is ours to contribute.
fn submit_captured_output_to_journal(id: String, output: &str) {
    let (text, truncated) = bounded_journal_output(output);
    if let Err(error) =
        crate::execution_journal::submit(crate::execution_journal::CompletedExecution {
            id,
            output: text,
            output_available: true,
            truncated,
            total_bytes: output.len(),
        })
    {
        log::debug!("jsh execution journal rejected a captured output: {error:?}");
    }
}

/// Bound a captured output to what the journal will accept, keeping the tail.
///
/// The tail, not the head: a failing command's diagnostic is at the end of its
/// output, and the head is still in the block itself. Cutting on a char boundary
/// matters because the journal is JSON and a split scalar would not encode.
fn bounded_journal_output(output: &str) -> (String, bool) {
    if output.len() <= MAX_JOURNAL_OUTPUT_BYTES {
        return (output.to_string(), false);
    }
    let start = output
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| output.len() - index <= MAX_JOURNAL_OUTPUT_BYTES)
        .unwrap_or(output.len());
    (output[start..].to_string(), true)
}

/// The command line a block records.
///
/// The shell's own metadata wins: it is what the shell parsed, whereas the
/// reconstruction is scraped off the rendered screen, where a redraw, a wrapped
/// line or an accepted autosuggestion can all make the text differ from what
/// ran. The reconstruction stays as the fallback for shells that emit the bare
/// FinalTerm mark with no parameters.
fn resolve_command_for_block(meta: &CommandMeta, reconstructed: &str) -> String {
    if let Some(command) = meta.command.as_deref().map(str::trim) {
        if !command.is_empty()
            && command.len() <= MAX_COMMAND_CAPTURE_BYTES
            && !command
                .chars()
                .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\t'))
            && !crate::review_input::contains_noncontrol_visual_spoof(command)
        {
            return command.to_string();
        }
    }
    let reconstructed = reconstructed.trim();
    if !reconstructed.is_empty() {
        return bounded_command_text(reconstructed);
    }
    if meta.command_truncated {
        return TRUNCATED_COMMAND_PLACEHOLDER.to_string();
    }
    String::new()
}

/// jterm4 truncates a multiline payload to its first line when the shell has not
/// advertised DECSET 2004, instead of letting every embedded newline execute a
/// line. A per-app product choice, not a bug; jterm2/jterm3 send verbatim.
const UNBRACKETED_MULTILINE: UnbracketedMultiline = UnbracketedMultiline::FirstLineOnly;

fn paste_modes(bracketed_paste: bool) -> PasteModes {
    PasteModes {
        bracketed: bracketed_paste,
    }
}

/// Encode a command this app is putting on the shell's prompt — block recall,
/// palette re-run, an agent suggestion.
///
/// The returned [`Paste`] carries both the bytes for the PTY (`Ctrl+U` first,
/// framing when the shell can strip it, an embedded `ESC[201~` always removed)
/// and `echo_text`, the text to mirror into the editor shadow so the shadow
/// cannot claim more than the child actually received.
///
/// The `Ctrl+U` is unconditional. Gating it on `pty_synced` appends the recalled
/// command to whatever the user had already typed, because typed text is not
/// represented by that flag.
pub(crate) fn build_command_recall(command: &str, bracketed_paste: bool) -> Paste {
    let command = command.trim_end_matches(['\r', '\n']);
    let modes = paste_modes(bracketed_paste);
    if command.len() > crate::review_input::MAX_REVIEW_INPUT_BYTES
        || crate::review_input::contains_noncontrol_visual_spoof(command)
    {
        // Do not return a bare Ctrl+U for rejected history: that would erase a
        // pending line even though no replacement text is safe to insert.
        return pty_input::encode_prompt_insert(
            "",
            modes,
            PastePolicy::prompt_insert(UNBRACKETED_MULTILINE),
            true,
        );
    }
    let mut policy = PastePolicy::prompt_insert(UNBRACKETED_MULTILINE);
    // The exact-pinned core revision still defaults prompt recall to preserving
    // controls. Captured OSC/history is not a trust boundary, so override it.
    policy.strip_controls = true;
    pty_input::encode_prompt_insert(command, modes, policy, true)
}

fn external_input_changes_editor(state: BlockState, data: &[u8]) -> bool {
    state == BlockState::AwaitingCommand && data.iter().any(|byte| !matches!(byte, b'\r' | b'\n'))
}

fn classify_command_prompt_status(
    state: BlockState,
    fullscreen: bool,
    idle_input_dirty: bool,
    pty_synced: bool,
    typed_command_empty: bool,
) -> CommandPromptStatus {
    if fullscreen || state == BlockState::AltScreen {
        return CommandPromptStatus::Fullscreen;
    }
    match state {
        BlockState::AwaitingCommand => {
            if idle_input_dirty || pty_synced || !typed_command_empty {
                CommandPromptStatus::HasInput
            } else {
                CommandPromptStatus::Ready
            }
        }
        BlockState::CollectingOutput | BlockState::PostCommand => CommandPromptStatus::Running,
        BlockState::RawFallback => CommandPromptStatus::ShellIntegrationUnavailable,
        BlockState::Idle | BlockState::CollectingPrompt => CommandPromptStatus::Initializing,
        BlockState::AltScreen => CommandPromptStatus::Fullscreen,
    }
}

/// Mirror input that bypasses VTE's `commit` signal (clipboard, Agent, and other
/// programmatic insertion) into the editor-state guards. Escape/control sequences
/// still mark the line dirty, but are not copied into the fallback command text.
fn record_external_input(
    state: BlockState,
    data: &[u8],
    typed_cmd: &RefCell<String>,
    pty_synced: &Cell<bool>,
    idle_input_dirty: &Cell<bool>,
) -> bool {
    if !external_input_changes_editor(state, data) {
        return false;
    }

    pty_synced.set(true);
    idle_input_dirty.set(true);

    if data == b"\x08" || data == b"\x7f" {
        pop_typed_command_shadow(&mut typed_cmd.borrow_mut());
        return true;
    }
    if data == b"\x15" {
        typed_cmd.borrow_mut().clear();
        return true;
    }

    let has_terminal_controls = data
        .iter()
        .any(|&byte| byte == 0x7f || (byte < 0x20 && !matches!(byte, b'\t' | b'\r' | b'\n')));
    if has_terminal_controls {
        return true;
    }

    let normalized = String::from_utf8_lossy(data)
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    append_typed_command_shadow(&mut typed_cmd.borrow_mut(), &normalized);
    true
}

/// Encode an approval-gated command and its submit key as one PTY queue item.
/// A bounded queue must never accept Enter after rejecting the command bytes.
fn approved_command_submission_payload(command: &str) -> Result<Vec<u8>, String> {
    crate::review_input::validate(command).map_err(|error| {
        format!("rejected unsafe programmatic command at the PTY boundary: {error}")
    })?;
    let capacity = command
        .len()
        .checked_add(1)
        .ok_or_else(|| "command length overflowed the PTY input size".to_string())?;
    if capacity > crate::pty::MAX_PTY_INPUT_MESSAGE_BYTES {
        return Err(format!(
            "command plus Enter exceeds the {}-byte PTY input limit",
            crate::pty::MAX_PTY_INPUT_MESSAGE_BYTES
        ));
    }
    let mut submission = Vec::with_capacity(capacity);
    submission.extend_from_slice(command.as_bytes());
    submission.push(b'\r');
    Ok(submission)
}

/// Encode clipboard text for the prompt.
///
/// Unlike a recall this is data from outside the app, so C0/C1 bytes are removed
/// as well (tab and newline survive). `echo_text` is what the fallback editor
/// model must mirror: it already reflects first-line truncation, so the shadow
/// cannot drift from what the child received.
fn build_clipboard_paste(text: &str, bracketed_paste: bool) -> Paste {
    pty_input::encode_paste(
        text,
        paste_modes(bracketed_paste),
        PastePolicy::clipboard(UNBRACKETED_MULTILINE),
    )
}

fn history_edge_navigation_available(state: BlockState, editor_dirty: bool) -> bool {
    !editor_dirty
        && !matches!(
            state,
            BlockState::CollectingOutput | BlockState::AltScreen | BlockState::RawFallback
        )
}

fn should_buffer_background_output(idle_input_dirty: bool, pty_synced: bool) -> bool {
    !idle_input_dirty && !pty_synced
}

/// Collect selected commands in terminal order, skipping background-only blocks.
fn selected_command_text<'a, I>(blocks: I, selected: &HashSet<u64>) -> String
where
    I: IntoIterator<Item = (u64, &'a str)>,
{
    let mut output = String::new();
    for (id, command) in blocks {
        if !selected.contains(&id) || command.trim().is_empty() {
            continue;
        }
        let separator = usize::from(!output.is_empty());
        let Some(next_len) = output
            .len()
            .checked_add(separator)
            .and_then(|length| length.checked_add(command.len()))
        else {
            return String::new();
        };
        if next_len > MAX_COMMAND_CAPTURE_BYTES {
            // Never insert a syntactically partial selection into the shell.
            return String::new();
        }
        if separator != 0 {
            output.push('\n');
        }
        output.push_str(command);
    }
    output
}

fn append_bounded_clipboard_section(output: &mut String, separator: &str, part: &str) -> bool {
    let Some(next_len) = output
        .len()
        .checked_add(separator.len())
        .and_then(|length| length.checked_add(part.len()))
    else {
        return false;
    };
    if next_len > MAX_SELECTED_CLIPBOARD_BYTES {
        return false;
    }
    output.push_str(separator);
    output.push_str(part);
    true
}

fn selected_clipboard_text<'a, I, F>(blocks: I, selected: &HashSet<u64>, mut render: F) -> String
where
    I: IntoIterator<Item = &'a BlockData>,
    F: FnMut(&BlockData) -> String,
{
    let mut output = String::new();
    for block in blocks {
        if !selected.contains(&block.id) {
            continue;
        }
        let part = render(block);
        let separator = if output.is_empty() { "" } else { "\n\n" };
        if !append_bounded_clipboard_section(&mut output, separator, &part) {
            // A partial multi-block copy can silently change its meaning.
            return String::new();
        }
    }
    output
}

fn recall_selected_commands_at_prompt(
    pty: &OwnedPty,
    pty_synced: &Cell<bool>,
    typed_cmd: &RefCell<String>,
    state: BlockState,
    finished: &[FinishedBlock],
    selected: &HashSet<u64>,
    bracketed_paste: bool,
) -> bool {
    let command = selected_command_text(
        finished
            .iter()
            .map(|block| (block.id, block.cmd_text.as_str())),
        selected,
    );
    recall_command_at_prompt(pty, pty_synced, typed_cmd, state, &command, bracketed_paste)
}

/// Replace the current shell edit buffer without executing the recalled command.
pub(crate) fn recall_command_at_prompt(
    pty: &OwnedPty,
    pty_synced: &Cell<bool>,
    typed_cmd: &RefCell<String>,
    state: BlockState,
    command: &str,
    bracketed_paste: bool,
) -> bool {
    if state != BlockState::AwaitingCommand {
        return false;
    }
    let paste = build_command_recall(command, bracketed_paste);
    if paste.is_empty() {
        return false;
    }
    // One write: the frame's start, body and end must not be split, and the
    // Ctrl+U rides in front of them (see `build_command_recall`).
    if let Err(error) = pty.write_bytes(&paste.bytes) {
        pty.report_write_error("could not queue recalled command", error);
        return false;
    }
    *typed_cmd.borrow_mut() = paste.echo_text;
    pty_synced.set(true);
    true
}

fn truncate_plain_output_for_height(output_plain: &str, line_limit: usize) -> (String, usize) {
    let trimmed = output_plain.trim();
    let total_lines = trimmed.lines().count();
    if total_lines <= line_limit {
        return (trimmed.to_string(), total_lines);
    }

    let kept = trimmed
        .lines()
        .take(line_limit)
        .collect::<Vec<_>>()
        .join("\n");
    let truncated = format!(
        "{}\n\n[... truncated: {} lines total, showing first {}]",
        kept, total_lines, line_limit
    );
    let displayed_lines = truncated.lines().count();
    (truncated, displayed_lines)
}

fn ansi256_to_rgb(idx: u8, palette: &[RGBA; 16]) -> (u8, u8, u8) {
    match idx {
        0..=15 => {
            let c = palette[idx as usize];
            (
                (c.red() * 255.0) as u8,
                (c.green() * 255.0) as u8,
                (c.blue() * 255.0) as u8,
            )
        }
        16..=231 => {
            let idx = idx - 16;
            let r = (idx / 36) * 51;
            let g = ((idx % 36) / 6) * 51;
            let b = (idx % 6) * 51;
            (r, g, b)
        }
        232..=255 => {
            let gray = 8 + (idx - 232) * 10;
            (gray, gray, gray)
        }
    }
}

/// Dynamic OSC 10/11/12 color overrides for one pane.
///
/// Set-sequences pass through the parser to the persistent live VTE, which
/// recolors itself natively — this struct only remembers the override so an
/// OSC `?` query ([`ParserEvent::ColorQuery`]) reports the color the app
/// actually sees instead of the static theme. OSC 110/111/112 clears a slot
/// back to the theme. Palette (OSC 4) sets are not tracked: VTE owns those
/// natively and the query fallback already derives them from the theme.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct DynamicColors {
    foreground: Option<RGBA>,
    background: Option<RGBA>,
    cursor: Option<RGBA>,
}

impl DynamicColors {
    /// Record a dynamic color set. Unparseable specs are ignored — the raw
    /// bytes still reached the live VTE, which applies its own leniency, so
    /// dropping the tracker update merely keeps the previous query answer.
    fn set(&mut self, kind: ColorKind, spec: &str) {
        let Some(rgba) = parse_color_spec(spec) else {
            return;
        };
        match kind {
            ColorKind::Foreground => self.foreground = Some(rgba),
            ColorKind::Background => self.background = Some(rgba),
            ColorKind::Cursor => self.cursor = Some(rgba),
            ColorKind::Palette(_) => {}
        }
    }

    /// Drop a dynamic color (OSC 110/111/112): queries fall back to the theme.
    fn reset(&mut self, kind: ColorKind) {
        match kind {
            ColorKind::Foreground => self.foreground = None,
            ColorKind::Background => self.background = None,
            ColorKind::Cursor => self.cursor = None,
            ColorKind::Palette(_) => {}
        }
    }

    fn get(&self, kind: ColorKind) -> Option<RGBA> {
        match kind {
            ColorKind::Foreground => self.foreground,
            ColorKind::Background => self.background,
            ColorKind::Cursor => self.cursor,
            ColorKind::Palette(_) => None,
        }
    }
}

/// Parse an OSC 10/11/12 color spec. Apps overwhelmingly send the X11
/// `rgb:R/G/B` form (1–4 hex digits per channel), which `gdk_rgba_parse`
/// does not understand; handle it here and delegate everything else
/// (`#RRGGBB` hex, CSS/X11 color names) to [`RGBA::parse`].
fn parse_color_spec(spec: &str) -> Option<RGBA> {
    let spec = spec.trim();
    if let Some(channels) = spec.strip_prefix("rgb:") {
        let mut it = channels.split('/');
        let (r, g, b) = (it.next()?, it.next()?, it.next()?);
        if it.next().is_some() {
            return None;
        }
        return Some(RGBA::new(
            parse_x11_channel(r)?,
            parse_x11_channel(g)?,
            parse_x11_channel(b)?,
            1.0,
        ));
    }
    RGBA::parse(spec).ok()
}

/// One `rgb:` channel: `f`, `ff`, `fff`, and `ffff` all mean full intensity —
/// the value scales against the widest value its digit count can express
/// (XParseColor semantics).
fn parse_x11_channel(digits: &str) -> Option<f32> {
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u16::from_str_radix(digits, 16).ok()?;
    let max = (1u32 << (4 * digits.len())) - 1;
    Some(value as f32 / max as f32)
}

fn build_color_query_reply(config: &Config, dynamic: DynamicColors, kind: ColorKind) -> String {
    let theme = match kind {
        ColorKind::Foreground => config.foreground,
        ColorKind::Background => config.background,
        ColorKind::Cursor => config.cursor,
        ColorKind::Palette(idx) => {
            let (r, g, b) = ansi256_to_rgb(idx, &config.palette);
            RGBA::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
        }
    };
    format_color_query_reply(kind, dynamic.get(kind).unwrap_or(theme))
}

/// Format the `\e]<n>;rgb:RRRR/GGGG/BBBB\e\\` reply for one resolved color.
fn format_color_query_reply(kind: ColorKind, rgba: RGBA) -> String {
    let r = (rgba.red() * 65535.0) as u16;
    let g = (rgba.green() * 65535.0) as u16;
    let b = (rgba.blue() * 65535.0) as u16;
    match kind {
        ColorKind::Foreground => format!("\x1b]10;rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\"),
        ColorKind::Background => format!("\x1b]11;rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\"),
        ColorKind::Cursor => format!("\x1b]12;rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\"),
        ColorKind::Palette(idx) => {
            format!("\x1b]4;{idx};rgb:{r:04x}/{g:04x}/{b:04x}\x1b\\")
        }
    }
}

/// Overlay tracked dynamic OSC 10/11 colors on a finished block's snapshot
/// VTEs. Snapshots are freshly themed from the static config, so a block that
/// finishes after an app recolored the terminal (OSC 11 theme switchers, vim
/// `background=`) would otherwise sit visibly mismatched next to the recolored
/// live view. The cursor slot is deliberately skipped: snapshot cursors are
/// hidden via a transparent cursor color (see `apply_snapshot_theme_to_vte`).
fn apply_dynamic_colors_to_finished(block: &FinishedBlock, dynamic: DynamicColors) {
    for vte in [&block.command_vte, &block.output_vte] {
        if let Some(fg) = dynamic.foreground {
            vte.set_color_foreground(&fg);
        }
        if let Some(bg) = dynamic.background {
            vte.set_color_background(&bg);
        }
    }
}

fn build_keyboard_query_reply(
    query: KeyboardProtocolQuery,
    cursor_col: i64,
    cursor_row: i64,
) -> String {
    match query {
        KeyboardProtocolQuery::KittyQuery => "\x1b[?0u".to_string(),
        KeyboardProtocolQuery::ModifyOtherKeysQuery => "\x1b[>4;0m".to_string(),
        KeyboardProtocolQuery::PrimaryDeviceAttributes => "\x1b[?1;2c".to_string(),
        KeyboardProtocolQuery::SecondaryDeviceAttributes => "\x1b[>0;0;0c".to_string(),
        KeyboardProtocolQuery::TertiaryDeviceAttributes => "\x1bP!|00000000\x1b\\".to_string(),
        KeyboardProtocolQuery::XtVersion => {
            format!("\x1bP>|jterm4 {}\x1b\\", env!("CARGO_PKG_VERSION"))
        }
        KeyboardProtocolQuery::DeviceStatus => "\x1b[0n".to_string(),
        KeyboardProtocolQuery::CursorPosition => format!(
            "\x1b[{};{}R",
            cursor_row.saturating_add(1).max(1),
            cursor_col.saturating_add(1).max(1)
        ),
    }
}

type SelectedBlockIds = Rc<RefCell<std::collections::HashSet<u64>>>;

#[derive(Clone, Copy)]
struct BlockSelectionRefs<'a> {
    ids: &'a SelectedBlockIds,
    active: &'a Rc<Cell<Option<u64>>>,
    anchor: &'a Rc<Cell<Option<u64>>>,
}

/// Apply the multi-selection model to every finished block. All selected blocks
/// get a light outline; the active edge owns the stronger outline, keyboard hint,
/// and persistent quick actions.
fn sync_finished_block_selection(
    finished: &[FinishedBlock],
    selected_block_ids: &SelectedBlockIds,
    selected_block_id: &Rc<Cell<Option<u64>>>,
) {
    let selected = selected_block_ids.borrow();
    let active = selected_block_id.get();
    for block in finished {
        let is_selected = selected.contains(&block.id);
        if is_selected {
            block.widget().add_css_class("block-selected");
        } else {
            block.widget().remove_css_class("block-selected");
        }

        let is_active = active == Some(block.id);
        block.selection_hint.set_visible(is_active);
        if is_active {
            block.widget().add_css_class("block-selection-active");
            block.action_box.set_visible(true);
        } else {
            block.widget().remove_css_class("block-selection-active");
            if !block.widget().has_css_class("block-hovered") {
                block.action_box.set_visible(false);
            }
        }
    }
}

fn clear_finished_block_selection(
    finished: &[FinishedBlock],
    selected_block_ids: &SelectedBlockIds,
    selected_block_id: &Rc<Cell<Option<u64>>>,
    selection_anchor_id: &Rc<Cell<Option<u64>>>,
) {
    selected_block_ids.borrow_mut().clear();
    selected_block_id.set(None);
    selection_anchor_id.set(None);
    sync_finished_block_selection(finished, selected_block_ids, selected_block_id);
}

fn clear_vte_text_selections(finished: &[FinishedBlock], active_vte: &Terminal) {
    active_vte.unselect_all();
    for block in finished {
        block.command_vte.unselect_all();
        block.output_vte.unselect_all();
    }
}

fn replace_finished_block_selection(
    finished: &[FinishedBlock],
    selected_block_ids: &SelectedBlockIds,
    selected_block_id: &Rc<Cell<Option<u64>>>,
    selection_anchor_id: &Rc<Cell<Option<u64>>>,
    new_id: Option<u64>,
) {
    let new_id = new_id.filter(|id| finished.iter().any(|block| block.id == *id));
    {
        let mut selected = selected_block_ids.borrow_mut();
        selected.clear();
        if let Some(id) = new_id {
            selected.insert(id);
        }
    }
    selected_block_id.set(new_id);
    selection_anchor_id.set(new_id);
    sync_finished_block_selection(finished, selected_block_ids, selected_block_id);
}

/// Make `id` the active edge without discarding an existing multi-selection.
fn activate_finished_block_selection(
    finished: &[FinishedBlock],
    selected_block_ids: &SelectedBlockIds,
    selected_block_id: &Rc<Cell<Option<u64>>>,
    selection_anchor_id: &Rc<Cell<Option<u64>>>,
    id: u64,
) {
    if !selected_block_ids.borrow().contains(&id) {
        replace_finished_block_selection(
            finished,
            selected_block_ids,
            selected_block_id,
            selection_anchor_id,
            Some(id),
        );
        return;
    }
    selected_block_id.set(Some(id));
    selection_anchor_id.set(Some(id));
    sync_finished_block_selection(finished, selected_block_ids, selected_block_id);
}

fn toggle_finished_block_selection(
    finished: &[FinishedBlock],
    selected_block_ids: &SelectedBlockIds,
    selected_block_id: &Rc<Cell<Option<u64>>>,
    selection_anchor_id: &Rc<Cell<Option<u64>>>,
    id: u64,
) {
    let removed = {
        let mut selected = selected_block_ids.borrow_mut();
        if selected.remove(&id) {
            true
        } else {
            selected.insert(id);
            false
        }
    };

    if removed {
        let active_missing = selected_block_id
            .get()
            .is_some_and(|active| !selected_block_ids.borrow().contains(&active));
        if selected_block_id.get() == Some(id) || active_missing {
            let fallback = {
                let selected = selected_block_ids.borrow();
                finished
                    .iter()
                    .rev()
                    .find(|block| selected.contains(&block.id))
                    .map(|block| block.id)
            };
            selected_block_id.set(fallback);
        }
        let anchor_missing = selection_anchor_id
            .get()
            .is_some_and(|anchor| !selected_block_ids.borrow().contains(&anchor));
        if selection_anchor_id.get() == Some(id) || anchor_missing {
            selection_anchor_id.set(selected_block_id.get());
        }
    } else {
        selected_block_id.set(Some(id));
        selection_anchor_id.set(Some(id));
    }
    sync_finished_block_selection(finished, selected_block_ids, selected_block_id);
}

fn selected_id_range(ids: &[u64], anchor: u64, target: u64) -> Vec<u64> {
    let Some(anchor_index) = ids.iter().position(|id| *id == anchor) else {
        return vec![target];
    };
    let Some(target_index) = ids.iter().position(|id| *id == target) else {
        return vec![target];
    };
    let (start, end) = if anchor_index <= target_index {
        (anchor_index, target_index)
    } else {
        (target_index, anchor_index)
    };
    ids[start..=end].to_vec()
}

fn select_finished_block_range(
    finished: &[FinishedBlock],
    selected_block_ids: &SelectedBlockIds,
    selected_block_id: &Rc<Cell<Option<u64>>>,
    selection_anchor_id: &Rc<Cell<Option<u64>>>,
    target: u64,
) {
    let anchor = selection_anchor_id
        .get()
        .or_else(|| selected_block_id.get())
        .unwrap_or(target);
    let ordered_ids: Vec<u64> = finished.iter().map(|block| block.id).collect();
    let range = selected_id_range(&ordered_ids, anchor, target);
    {
        let mut selected = selected_block_ids.borrow_mut();
        selected.clear();
        selected.extend(range);
    }
    selected_block_id.set(Some(target));
    selection_anchor_id.set(Some(anchor));
    sync_finished_block_selection(finished, selected_block_ids, selected_block_id);
}

fn remove_finished_block_from_selection(
    finished: &[FinishedBlock],
    selected_block_ids: &SelectedBlockIds,
    selected_block_id: &Rc<Cell<Option<u64>>>,
    selection_anchor_id: &Rc<Cell<Option<u64>>>,
    removed_id: u64,
) {
    selected_block_ids.borrow_mut().remove(&removed_id);
    let active_missing = selected_block_id
        .get()
        .is_some_and(|active| !selected_block_ids.borrow().contains(&active));
    if selected_block_id.get() == Some(removed_id) || active_missing {
        let fallback = {
            let selected = selected_block_ids.borrow();
            finished
                .iter()
                .rev()
                .find(|block| selected.contains(&block.id))
                .map(|block| block.id)
        };
        selected_block_id.set(fallback);
    }
    let anchor_missing = selection_anchor_id
        .get()
        .is_some_and(|anchor| !selected_block_ids.borrow().contains(&anchor));
    if selection_anchor_id.get() == Some(removed_id) || anchor_missing {
        selection_anchor_id.set(selected_block_id.get());
    }
    sync_finished_block_selection(finished, selected_block_ids, selected_block_id);
}

/// Reveal a selected block with the smallest possible scroll movement. The old
/// navigation path always moved the block to one-third of the viewport, making
/// repeated Ctrl+Shift+Up/Down feel like the document jumped under the cursor.
fn scroll_finished_block_into_view(scroll: &ScrolledWindow, block: &FinishedBlock) {
    let scroll = scroll.clone();
    let widget = block.widget().clone();
    glib::idle_add_local_once(move || {
        let Some(bounds) = widget.compute_bounds(&scroll) else {
            return;
        };
        let adj = scroll.vadjustment();
        let viewport_height = adj.page_size().max(scroll.height() as f64);
        let delta = scroll_delta_to_reveal(
            bounds.y() as f64,
            (bounds.y() + bounds.height()) as f64,
            viewport_height,
            18.0,
        );
        if delta.abs() < 1.0 {
            return;
        }
        let max_value = (adj.upper() - adj.page_size()).max(adj.lower());
        adj.set_value((adj.value() + delta).clamp(adj.lower(), max_value));
    });
}

fn scroll_delta_to_reveal(top: f64, bottom: f64, viewport_height: f64, padding: f64) -> f64 {
    if viewport_height <= 1.0 {
        return 0.0;
    }
    let padding = padding.clamp(0.0, viewport_height / 4.0);
    let usable_height = (viewport_height - padding * 2.0).max(1.0);
    if bottom - top >= usable_height || top < padding {
        top - padding
    } else if bottom > viewport_height - padding {
        bottom - (viewport_height - padding)
    } else {
        0.0
    }
}

/// HOME/END move through the outer history canvas. END repeats briefly because
/// virtualized blocks can regain height as they enter the viewport.
fn scroll_history_to_edge(scroll: &ScrolledWindow, bottom: bool) {
    let adj = scroll.vadjustment();
    if !bottom {
        adj.set_value(adj.lower());
        return;
    }
    adj.set_value((adj.upper() - adj.page_size()).max(adj.lower()));
    let scroll = scroll.clone();
    let tries = Rc::new(Cell::new(0u8));
    let stable_turns = Rc::new(Cell::new(0u8));
    let last_target = Rc::new(Cell::new(None::<f64>));
    glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
        tries.set(tries.get().saturating_add(1));
        let adj = scroll.vadjustment();
        let target = (adj.upper() - adj.page_size()).max(adj.lower());
        adj.set_value(target);

        let target_is_stable = last_target
            .get()
            .is_some_and(|previous| (previous - target).abs() < 1.0);
        last_target.set(Some(target));
        if target_is_stable && (adj.value() - target).abs() < 1.0 {
            stable_turns.set(stable_turns.get().saturating_add(1));
        } else {
            stable_turns.set(0);
        }

        if stable_turns.get() >= 2 || tries.get() >= 12 {
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
}

fn move_finished_block_selection(
    finished: &[FinishedBlock],
    selected_block_ids: &SelectedBlockIds,
    selected_block_id: &Rc<Cell<Option<u64>>>,
    selection_anchor_id: &Rc<Cell<Option<u64>>>,
    scroll: &ScrolledWindow,
    direction: i32,
) -> bool {
    if finished.is_empty() || direction == 0 {
        return false;
    }
    let current = selected_block_id
        .get()
        .and_then(|id| finished.iter().position(|block| block.id == id));
    let target = if direction < 0 {
        match current {
            None => Some(finished.len() - 1),
            Some(0) => Some(0),
            Some(index) => Some(index - 1),
        }
    } else {
        match current {
            None => return false,
            Some(index) if index + 1 >= finished.len() => None,
            Some(index) => Some(index + 1),
        }
    };
    let target_id = target.and_then(|index| finished.get(index).map(|block| block.id));
    replace_finished_block_selection(
        finished,
        selected_block_ids,
        selected_block_id,
        selection_anchor_id,
        target_id,
    );
    if let Some(index) = target {
        if let Some(block) = finished.get(index) {
            scroll_finished_block_into_view(scroll, block);
        }
    }
    true
}

fn extend_finished_block_selection(
    finished: &[FinishedBlock],
    selected_block_ids: &SelectedBlockIds,
    selected_block_id: &Rc<Cell<Option<u64>>>,
    selection_anchor_id: &Rc<Cell<Option<u64>>>,
    scroll: &ScrolledWindow,
    direction: i32,
) -> bool {
    if finished.is_empty() || direction == 0 {
        return false;
    }
    let Some(current) = selected_block_id
        .get()
        .and_then(|id| finished.iter().position(|block| block.id == id))
    else {
        return false;
    };
    let target = if direction < 0 {
        current.saturating_sub(1)
    } else {
        (current + 1).min(finished.len() - 1)
    };
    let Some(block) = finished.get(target) else {
        return false;
    };
    select_finished_block_range(
        finished,
        selected_block_ids,
        selected_block_id,
        selection_anchor_id,
        block.id,
    );
    scroll_finished_block_into_view(scroll, block);
    true
}

fn scroll_selected_finished_block_edge(
    finished: &[FinishedBlock],
    selected_block_id: &Rc<Cell<Option<u64>>>,
    scroll: &ScrolledWindow,
    bottom: bool,
) -> bool {
    let Some(id) = selected_block_id.get() else {
        return false;
    };
    let Some(block) = finished.iter().find(|block| block.id == id) else {
        return false;
    };
    block.scroll_to_edge(scroll, bottom);
    true
}

/// Remove one block and all of its parallel state. Keeping the GTK widgets,
/// serializable history, selection, and bookmarks in lockstep prevents deleted
/// blocks from reappearing in history/search or leaving stale keyboard targets.
/// Returns the nearest surviving block so repeated Delete presses can keep going.
fn remove_finished_block(
    block_id: u64,
    finished_blocks: &Rc<RefCell<Vec<FinishedBlock>>>,
    block_data: &Rc<RefCell<VecDeque<BlockData>>>,
    block_list: &gtk4::Box,
    selection: BlockSelectionRefs<'_>,
    bookmarks: &Rc<RefCell<std::collections::HashSet<u64>>>,
    visible_indices: &Rc<RefCell<std::collections::HashSet<usize>>>,
) -> Option<u64> {
    let removed = {
        let mut finished = finished_blocks.borrow_mut();
        finished
            .iter()
            .position(|b| b.id == block_id)
            .map(|pos| (pos, finished.remove(pos)))
    };
    let (removed_pos, block) = removed?;

    block_list.remove(block.widget());
    block_data.borrow_mut().retain(|b| b.id != block_id);
    bookmarks.borrow_mut().remove(&block_id);
    // Virtual-scroll visibility is index-based, so shift every surviving index
    // above the removed position down by one.
    let mut visible = visible_indices.borrow_mut();
    let shifted = visible
        .iter()
        .filter_map(|&i| {
            if i == removed_pos {
                None
            } else if i > removed_pos {
                Some(i - 1)
            } else {
                Some(i)
            }
        })
        .collect();
    *visible = shifted;
    let finished = finished_blocks.borrow();
    remove_finished_block_from_selection(
        &finished,
        selection.ids,
        selection.active,
        selection.anchor,
        block_id,
    );
    finished
        .get(removed_pos)
        .or_else(|| {
            removed_pos
                .checked_sub(1)
                .and_then(|previous| finished.get(previous))
        })
        .map(|block| block.id)
}

/// Install the shared click-to-select behavior for a finished block. New blocks
/// and restored history blocks must use the same handler; otherwise keyboard
/// block actions only work on commands produced after app startup.
fn install_finished_block_selection(
    block: &FinishedBlock,
    active: &Rc<RefCell<ActiveBlock>>,
    finished_blocks: &Rc<RefCell<Vec<FinishedBlock>>>,
    selected_block_ids: &SelectedBlockIds,
    selected_block_id: &Rc<Cell<Option<u64>>>,
    selection_anchor_id: &Rc<Cell<Option<u64>>>,
) {
    let active_for_click = Rc::downgrade(active);
    let header_for_click = block.header_row.clone();
    let finished_blocks_for_select = Rc::downgrade(finished_blocks);
    let selected_ids_for_click = selected_block_ids.clone();
    let selected_for_click = selected_block_id.clone();
    let anchor_for_click = selection_anchor_id.clone();
    let this_id = block.id;
    let left_click = gtk4::GestureClick::new();
    left_click.set_button(1);
    left_click.set_propagation_phase(gtk4::PropagationPhase::Capture);
    left_click.connect_pressed(move |gesture, n_press, _, y| {
        if n_press != 1 {
            gesture.set_state(gtk4::EventSequenceState::Denied);
            return;
        }
        let state = gesture.current_event_state();
        let ctrl = state.contains(gtk4::gdk::ModifierType::CONTROL_MASK);
        let shift = state.contains(gtk4::gdk::ModifierType::SHIFT_MASK);
        let over_terminal_surface = y > header_for_click.height() as f64;
        let Some(finished_blocks_for_select) = finished_blocks_for_select.upgrade() else {
            return;
        };
        let finished = finished_blocks_for_select.borrow();
        if over_terminal_surface && !shift {
            // A normal click/drag in a snapshot VTE means text interaction, not
            // whole-card interaction. Clear stale keyboard/card selection so
            // Ctrl+Shift+C copies the visibly selected text and Enter cannot
            // unexpectedly recall an older command.
            if selected_for_click.get().is_some() {
                clear_finished_block_selection(
                    &finished,
                    &selected_ids_for_click,
                    &selected_for_click,
                    &anchor_for_click,
                );
            }
        } else {
            if let Some(active_for_click) = active_for_click.upgrade() {
                active_for_click.borrow().grab_focus();
            }
            if ctrl && shift {
                toggle_finished_block_selection(
                    &finished,
                    &selected_ids_for_click,
                    &selected_for_click,
                    &anchor_for_click,
                    this_id,
                );
            } else if shift {
                select_finished_block_range(
                    &finished,
                    &selected_ids_for_click,
                    &selected_for_click,
                    &anchor_for_click,
                    this_id,
                );
            } else {
                replace_finished_block_selection(
                    &finished,
                    &selected_ids_for_click,
                    &selected_for_click,
                    &anchor_for_click,
                    Some(this_id),
                );
            }
        }
        gesture.set_state(if shift && over_terminal_surface {
            gtk4::EventSequenceState::Claimed
        } else {
            gtk4::EventSequenceState::Denied
        });
    });
    block.widget().add_controller(left_click);
}

fn popdown_if_alive(popover: &glib::WeakRef<gtk4::Popover>) {
    if let Some(popover) = popover.upgrade() {
        popover.popdown();
    }
}

/// Cap on the retained raw output buffer for a single running command. The raw
/// byte buffer used to re-render the finished block grew without bound — a runaway
/// command (`cat /dev/urandom`) could exhaust memory before CommandEnd. When the
/// buffer exceeds this, the oldest bytes are dropped, keeping the most recent tail
/// (the part a finished block actually shows). 8 MiB comfortably covers any normal
/// command's output.
const MAX_RAW_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

/// Visual floor for the live prompt/editor while no command owns the screen.
/// This does not become the PTY's row count: the child always receives the full
/// viewport winsize via `pty_grid_size`.
const MIN_INPUT_ROWS: i32 = 6;

/// `(command, exit status, output sample)`. The status is `None` when the shell
/// reported none, so every observer has to decide what that means for it rather
/// than being handed a 0 that reads as success.
type BlockFinishedCallbacks =
    Rc<RefCell<Vec<Box<dyn Fn(String, Option<i32>, String, Option<u64>)>>>>;

pub struct TermView {
    root: gtk4::Box,
    /// Status strip above the block list, shown only while this pane's tab
    /// is split.
    pane_header: crate::ui::PaneHeader,
    block_scroll: ScrolledWindow,
    block_list: gtk4::Box,
    jump_fab: gtk4::Button,
    unread_count: Rc<Cell<u32>>,
    /// The single persistent live VTE (jterm1 model): prompt + typing + output all
    /// render here natively; finished commands snapshot into styled blocks above.
    active_vte: Terminal,
    active: Rc<RefCell<ActiveBlock>>,
    bstate: Rc<Cell<BlockState>>,
    #[allow(dead_code)]
    prompt_buf: Rc<RefCell<String>>,
    /// Keystroke shadow used only as a fallback command capture. The authoritative
    /// finished-command text is read off the live VTE at CommandStart.
    #[allow(dead_code)]
    typed_cmd: Rc<RefCell<String>>,
    /// Exact command supplied by an execution-approved programmatic path. It is
    /// consumed at CommandStart before the VTE capture, which may still show a
    /// previous line when submission outruns display rendering.
    external_submission: Rc<RefCell<Option<String>>>,
    /// Programmatic input and native VTE commits both set this while the current
    /// prompt has been edited. It prevents background output and Agent insertion
    /// from treating a non-empty readline buffer as clean.
    idle_input_dirty: Rc<Cell<bool>>,
    /// True while an alt-screen app owns the viewport (finished blocks hidden).
    fullscreen: Rc<Cell<bool>>,
    /// True once the user has scrolled up off the live prompt; while false the
    /// view follows the bottom. Read by the per-frame tick to re-pin the prompt.
    #[allow(dead_code)]
    user_scrolled_up: Rc<Cell<bool>>,
    /// Guards programmatic scrolls so the scroll-lock detector doesn't mistake
    /// them for a user drag.
    #[allow(dead_code)]
    programmatic_scroll: Rc<Cell<bool>>,
    /// The one coalesced, frame-spaced follow-bottom controller shared by output
    /// updates and tab activation. Sharing it prevents activation from racing a
    /// still-running output/layout pin.
    scroll_debouncer: ScrollDebouncer,
    pty: Rc<OwnedPty>,
    pty_synced: Rc<Cell<bool>>,
    cwd_callbacks: StrCallbacks,
    remote_session_callbacks: StrCallbacks,
    exited_callbacks: IntCallbacks,
    bell_callbacks: VoidCallbacks,
    title_callbacks: StrCallbacks,
    activity_callbacks: VoidCallbacks,
    mouse_reporting_mode: Rc<Cell<MouseReportingMode>>,
    /// Whether the shell has enabled DECSET 2004. Clipboard input is written
    /// directly to our PTY, so block mode must apply this wrapper itself.
    bracketed_paste: Rc<Cell<bool>>,
    config: Rc<RefCell<Config>>,
    block_data: Rc<RefCell<VecDeque<BlockData>>>,
    finished_blocks: Rc<RefCell<Vec<FinishedBlock>>>,
    widget_pool: Rc<RefCell<WidgetPool>>,
    viewport: Rc<RefCell<ViewportState>>,
    visible_indices: Rc<RefCell<std::collections::HashSet<usize>>>,
    selected_block_ids: SelectedBlockIds,
    selected_block_id: Rc<Cell<Option<u64>>>,
    selection_anchor_id: Rc<Cell<Option<u64>>>,
    bookmarks: Rc<RefCell<std::collections::HashSet<u64>>>,
    /// Find-within-blocks state: every match across the finished blocks plus a
    /// cursor into it, so Ctrl+F highlights all hits and Next/Prev step through
    /// them (Warp's FindWithinBlock). Tags are stripped on close via clear_find.
    find_state: Rc<RefCell<FindState>>,
    current_cwd: Rc<RefCell<String>>,
    /// The tab's persistent session id (window snapshot `sid`). Keys the
    /// per-tab block-history file so concurrent tabs never overwrite each
    /// other's saved history.
    session_id: Option<String>,
    /// Send-safe handoff for the worker-decoded history and the race between a
    /// delayed load, live commands, clear, and shutdown save.
    history_load: Arc<history::HistoryLoadShared>,
    /// False only for a pane prepared inside a restore transaction which later
    /// aborted. Such a never-visible pane must not overwrite the real session's
    /// history merely because its controller is being rolled back.
    persist_history_on_drop: Cell<bool>,
    /// Main-thread poll applying a completed history load to GTK widgets.
    history_load_poll_id: RefCell<Option<glib::SourceId>>,
    /// Per-frame resize tick installed on `active_vte`. Held so it can be removed on
    /// Drop — otherwise the callback runs forever and keeps its Rc captures
    /// (pty/active/vte/vte_box) alive past tab close.
    resize_tick_id: RefCell<Option<gtk4::TickCallbackId>>,
    /// Periodic sticky-header refresh. Remove it explicitly on tab close so its
    /// GTK captures cannot retain a detached block tree.
    sticky_timer_id: RefCell<Option<glib::SourceId>>,
    /// Tracks per-VTE selections so a drag that crosses block boundaries can be
    /// copied as one contiguous string via Ctrl+Shift+C.
    cross_selection: Rc<CrossSelection>,
    block_finished_callbacks: BlockFinishedCallbacks,
    /// Parks PTY output while the user drag-selects on the live VTE; released
    /// on copy/typing/timeout. Kept here so input and copy paths can resume
    /// the feed immediately.
    selection_feed_hold: Rc<SelectionFeedHold>,
}

impl Drop for TermView {
    fn drop(&mut self) {
        if self.persist_history_on_drop.get() {
            if let Err(err) = self.save_history() {
                log::warn!("save block history on close: {err}");
            }
        }
        if let Some(id) = self.history_load_poll_id.borrow_mut().take() {
            id.remove();
        }
        if let Some(id) = self.resize_tick_id.borrow_mut().take() {
            id.remove();
        }
        if let Some(id) = self.sticky_timer_id.borrow_mut().take() {
            id.remove();
        }
    }
}

/// Captures the shared handles the PTY reader/exit callbacks need, so
/// `TermView::new` does not carry the reader closure inline.
struct ReaderCtx {
    active_rc: Rc<RefCell<ActiveBlock>>,
    /// The live VTE — every byte is fed here; alt-screen toggles feed it 1049h/l.
    active_vte: Terminal,
    bstate_rc: Rc<Cell<BlockState>>,
    /// State to restore when an alt-screen app exits (jterm1 model).
    prev_state_rc: Rc<Cell<BlockState>>,
    osc133_depth_rc: Rc<Cell<u32>>,
    prompt_buf_rc: Rc<RefCell<String>>,
    /// Keystroke-shadow input line, used only as a fallback if the VTE-text
    /// capture at CommandStart returns empty.
    typed_cmd_rc: Rc<RefCell<String>>,
    /// Exact command from an approved programmatic submission, if any.
    external_submission_rc: Rc<RefCell<Option<String>>>,
    /// Bytes emitted asynchronously after PromptEnd and before the next PromptStart.
    /// Empty-command blocks are inferred from this separate buffer, so no history
    /// schema change is needed.
    background_output_rc: Rc<RefCell<BoundedByteRing>>,
    /// Once the user starts editing at an idle prompt, output is intentionally left
    /// inline: shell echo/completion and true background output are ambiguous then.
    idle_input_dirty_rc: Rc<Cell<bool>>,
    /// Command text read from the live VTE at CommandStart; primary source
    /// for the finished block.
    vte_typed_cmd_rc: Rc<RefCell<String>>,
    /// VTE cursor position (col, row) captured at PromptEnd; the start anchor
    /// for the text-range read that produces `vte_typed_cmd_rc`.
    prompt_end_pos_rc: Rc<Cell<(i64, i64)>>,
    /// Rendered prompt (last non-empty line) captured at PromptEnd, used by the
    /// finalize path since prompt_buf is cleared once the prompt ends.
    prompt_display_rc: Rc<RefCell<String>>,
    block_list_rc: gtk4::Box,
    block_scroll_rc: ScrolledWindow,
    remote_session_cbs: StrCallbacks,
    exited_cbs: IntCallbacks,
    activity_cbs: VoidCallbacks,
    mouse_reporting_rc: Rc<Cell<MouseReportingMode>>,
    bracketed_paste_rc: Rc<Cell<bool>>,
    config_for_cb: Rc<RefCell<Config>>,
    parser: Rc<RefCell<Parser>>,
    block_data_for_cb: Rc<RefCell<VecDeque<BlockData>>>,
    finished_blocks_for_cb: Rc<RefCell<Vec<FinishedBlock>>>,
    scroll_debouncer: ScrollDebouncer,
    widget_pool_for_cb: Rc<RefCell<WidgetPool>>,
    pty_synced_rc: Rc<Cell<bool>>,
    visible_indices_rc: Rc<RefCell<std::collections::HashSet<usize>>>,
    fullscreen_rc: Rc<Cell<bool>>,
    ftcs_seen_rc: Rc<Cell<bool>>,
    init_cmds_queue_for_cb: Rc<RefCell<std::collections::VecDeque<String>>>,
    pty_for_init: Rc<OwnedPty>,
    block_start_time_for_cb: Rc<Cell<Option<SystemTime>>>,
    /// `None` means the shell reported no exit status for the finished command.
    /// It must not read as a successful 0.
    pending_exit_code_rc: Rc<Cell<Option<i32>>>,
    /// OSC 133 metadata for the command currently running, if the shell sends any.
    pending_command_meta_rc: Rc<RefCell<PendingCommandMeta>>,
    current_cwd_for_cb: Rc<RefCell<String>>,
    event_buf: Rc<RefCell<Vec<ParserEvent>>>,
    unread_count_rc: Rc<Cell<u32>>,
    jump_fab: gtk4::Button,
    sticky_bar: gtk4::Box,
    selected_block_ids_rc: SelectedBlockIds,
    selected_block_id_rc: Rc<Cell<Option<u64>>>,
    selection_anchor_id_rc: Rc<Cell<Option<u64>>>,
    bookmarks_for_cb: Rc<RefCell<std::collections::HashSet<u64>>>,
    cmd_running_rc: Rc<Cell<bool>>,
    running_cmd_rc: Rc<RefCell<String>>,
    /// Switches the live surface between compact prompt and full-screen layouts.
    /// PTY geometry is deliberately synchronized separately.
    layout_active_surface: Rc<dyn Fn()>,
    /// Bottom-of-pane repo metadata label. Re-probed every time a block
    /// finishes (the user may have just run `git commit`, `git pull`,
    /// or anything else that changes branch/dirty/ahead-behind).
    repo_strip: gtk4::Label,
    block_finished_cbs: BlockFinishedCallbacks,
    /// Parks incoming PTY chunks while the user drag-selects text on the live
    /// VTE, so streaming repaints can't destroy the selection mid-drag.
    selection_feed_hold: Rc<SelectionFeedHold>,
}

/// Fold every run of consecutive `ParserEvent::Bytes(_)` entries in `events`
/// into a single Bytes event whose payload is the concatenation. Preserves
/// the relative order of all other event kinds. The reader callback dispatches
/// per-event side effects (active_vte.feed, mark_dirty, accumulate_output,
/// activity_cbs), so coalescing replaces N feeds + N mark_dirty calls inside
/// one chunk with one of each per stretch — a win on `top` redraws, `cargo
/// build` spew, and any sustained byte-only output. Safe because boundary
/// events (PromptStart/End, AltScreen*, CommandStart/End) are NOT merged and
/// keep their own synchronous mark_dirty.
fn coalesce_bytes_events(events: &mut Vec<ParserEvent>) {
    if events.len() < 2 {
        return;
    }
    let mut write = 0usize;
    let mut i = 0usize;
    let n = events.len();
    while i < n {
        if matches!(events[i], ParserEvent::Bytes(_)) {
            // Move the first Bytes payload out so we can extend it in place.
            let placeholder = ParserEvent::Bytes(Vec::new());
            let first = std::mem::replace(&mut events[i], placeholder);
            let mut merged = match first {
                ParserEvent::Bytes(b) => b,
                _ => unreachable!(),
            };
            i += 1;
            while i < n {
                if let ParserEvent::Bytes(b) = &events[i] {
                    merged.extend_from_slice(b);
                    i += 1;
                } else {
                    break;
                }
            }
            events[write] = ParserEvent::Bytes(merged);
            write += 1;
        } else {
            if write != i {
                events.swap(write, i);
            }
            write += 1;
            i += 1;
        }
    }
    events.truncate(write);
}

fn is_post_command_metadata(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x1b]7;")
        || bytes.starts_with(b"\x1b]0;")
        || bytes.starts_with(b"\x1b]1;")
        || bytes.starts_with(b"\x1b]2;")
}

/// Background output is meaningful only when stripping terminal decoration leaves
/// at least one visible character. Prompt redraw control sequences and blank CR/LF
/// bursts should not create empty history cards.
fn background_output_has_visible_text(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    strip_ansi(text.as_ref())
        .chars()
        .any(|ch| !ch.is_whitespace() && !ch.is_control())
}

fn take_background_output(pending: &RefCell<BoundedByteRing>) -> Option<String> {
    let bytes = pending.borrow_mut().take_vec();
    background_output_has_visible_text(&bytes).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// Minimum spacing between OSC 9/777 desktop notifications. The sequence
/// originates inside the PTY (and may be remote over SSH), so process
/// spawning is rate-limited app-wide: the first allowed notification also
/// refreshes the timestamp, which drops the rest of a burst.
const NOTIFICATION_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

thread_local! {
    /// Last desktop notification launch, shared by every pane on the GLib
    /// main thread so the rate limit above holds across the whole app.
    static LAST_NOTIFICATION_AT: Cell<Option<std::time::Instant>> = const { Cell::new(None) };
}

/// Pure decision behind [`NOTIFICATION_MIN_INTERVAL`]: a notification may
/// fire when none has fired yet or the previous one is old enough.
fn notification_allowed(last: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    last.is_none_or(|prev| now.duration_since(prev) >= NOTIFICATION_MIN_INTERVAL)
}

impl ReaderCtx {
    fn install(self, pty: &Rc<OwnedPty>) {
        let ReaderCtx {
            active_rc,
            active_vte,
            bstate_rc,
            prev_state_rc,
            osc133_depth_rc,
            prompt_buf_rc,
            typed_cmd_rc,
            external_submission_rc,
            background_output_rc,
            idle_input_dirty_rc,
            vte_typed_cmd_rc,
            prompt_end_pos_rc,
            prompt_display_rc,
            block_list_rc,
            block_scroll_rc,
            remote_session_cbs,
            exited_cbs,
            activity_cbs,
            mouse_reporting_rc,
            bracketed_paste_rc,
            config_for_cb,
            parser,
            block_data_for_cb,
            finished_blocks_for_cb,
            scroll_debouncer,
            widget_pool_for_cb,
            pty_synced_rc,
            visible_indices_rc,
            fullscreen_rc,
            ftcs_seen_rc,
            init_cmds_queue_for_cb,
            pty_for_init,
            block_start_time_for_cb,
            pending_exit_code_rc,
            pending_command_meta_rc,
            current_cwd_for_cb,
            event_buf,
            unread_count_rc,
            jump_fab,
            sticky_bar,
            selected_block_ids_rc,
            selected_block_id_rc,
            selection_anchor_id_rc,
            bookmarks_for_cb,
            cmd_running_rc,
            running_cmd_rc,
            layout_active_surface,
            repo_strip,
            block_finished_cbs,
            selection_feed_hold,
        } = self;
        let active_alt_screen_mode_rc: Rc<Cell<Option<u32>>> = Rc::new(Cell::new(None));
        // Kitty graphics (APC G) — multi-chunk uploads assemble here; completed
        // textures wait against the running command until its block finishes.
        // The byte counter enforces the shared per-block budget so a runaway
        // shell cannot balloon RSS between prompts.
        let kitty_assembler_rc: Rc<RefCell<kitty_graphics::Assembler>> =
            Rc::new(RefCell::new(kitty_graphics::Assembler::new()));
        let kitty_pending_images_rc: Rc<RefCell<Vec<gtk4::gdk::Texture>>> =
            Rc::new(RefCell::new(Vec::new()));
        let kitty_pending_bytes_rc: Rc<Cell<usize>> = Rc::new(Cell::new(0));
        // Dynamic OSC 10/11/12 colors: the set/reset bytes pass through to the
        // persistent live VTE, which recolors itself natively; this cell only
        // remembers the override so ColorQuery replies (and freshly created
        // finished-block snapshots) match what the app actually set.
        let dynamic_colors_rc: Rc<Cell<DynamicColors>> =
            Rc::new(Cell::new(DynamicColors::default()));
        // Selection-hold triggers that live on the VTE itself (selection
        // cleared, user typed). Wired before the pipeline closure below takes
        // ownership of `active_vte`.
        selection_feed_hold.install_vte_hooks(&active_vte);

        // The whole per-chunk pipeline (parser → state machine → live-VTE
        // feed) behind one re-callable handle, so the selection feed-hold can
        // replay parked chunks through the exact path it intercepted them from.
        let process_chunk: Rc<RefCell<dyn FnMut(Vec<u8>)>> = Rc::new(RefCell::new(
            move |data: Vec<u8>| {
                let mut events = event_buf.borrow_mut();
                events.clear();
                parser.borrow_mut().feed(&data, &mut events);
                // Fold runs of consecutive `Bytes` events into one so the live
                // VTE feed, autoscroll mark-dirty, and accumulate_output happen
                // once per stretch instead of once per parser chunk. Boundary
                // events (PromptStart/End, AltScreen*, CommandStart/End) still
                // run their synchronous mark_dirty between stretches, keeping
                // the scroll-invariant from [[scroll_synchronous_autoscroll]].
                coalesce_bytes_events(&mut events);

                for event in events.iter() {
                    let state = bstate_rc.get();
                    match event {
                        ParserEvent::DecsetMode { mode, set } => {
                            if *mode == 2004 {
                                bracketed_paste_rc.set(*set);
                                // The PTY write boundary needs the same answer.
                                // Feeding it from here makes this parser the one
                                // owner of DECSET 2004 for the pane; the raw byte
                                // scan the reader thread used to run could only
                                // approximate a sequence split across chunks.
                                pty_for_init.set_shell_bracketed_paste(*set);
                            }
                            // VTE handles paste/cursor/etc. natively from its
                            // own bytes; block_view only needs mouse-reporting
                            // state for wheel suppression in alt-screen apps.
                            let new_mode = match (*mode, *set) {
                                (1000, true) => Some(MouseReportingMode::Click),
                                (1002, true) => Some(MouseReportingMode::Button),
                                (1003, true) => Some(MouseReportingMode::Motion),
                                (1006, true) => Some(MouseReportingMode::Sgr),
                                (1000 | 1002 | 1003 | 1006, false) => {
                                    Some(MouseReportingMode::None)
                                }
                                _ => None,
                            };
                            if let Some(m) = new_mode {
                                mouse_reporting_rc.set(m);
                            }
                        }
                        ParserEvent::Bytes(bytes) => {
                            // No shell integration seen yet: once real output flows,
                            // stream everything into the live VTE (raw fallback).
                            if state == BlockState::Idle {
                                bstate_rc.set(BlockState::RawFallback);
                            }

                            let feed_active_vte = match bstate_rc.get() {
                                BlockState::CollectingPrompt => {
                                    let text = String::from_utf8_lossy(bytes);
                                    append_bounded_text_tail(
                                        &mut prompt_buf_rc.borrow_mut(),
                                        &text,
                                        MAX_PROMPT_CAPTURE_BYTES,
                                    );
                                    scroll_debouncer.mark_dirty(&block_scroll_rc);
                                    true
                                }
                                BlockState::AwaitingCommand => {
                                    // Warp separates asynchronous output only when it
                                    // arrives before the user begins editing. Once input
                                    // is dirty, PTY echo/completion is indistinguishable
                                    // from a background process and remains inline.
                                    if should_buffer_background_output(
                                        idle_input_dirty_rc.get(),
                                        pty_synced_rc.get(),
                                    ) {
                                        background_output_rc.borrow_mut().append(bytes);
                                    }
                                    scroll_debouncer.mark_dirty(&block_scroll_rc);
                                    true
                                }
                                BlockState::CollectingOutput | BlockState::PostCommand => {
                                    if bstate_rc.get() != BlockState::PostCommand
                                        || !is_post_command_metadata(bytes)
                                    {
                                        active_rc.borrow().accumulate_output(bytes);
                                    }
                                    for cb in activity_cbs.borrow().iter() {
                                        cb();
                                    }
                                    true
                                }
                                BlockState::AltScreen => {
                                    // Alt-screen bytes go to the live VTE only — they
                                    // are not captured into block output (ephemeral).
                                    true
                                }
                                _ => true,
                            };

                            if feed_active_vte {
                                active_vte.feed(bytes);
                            }
                        }

                        ParserEvent::PromptStart => {
                            ftcs_seen_rc.set(true);
                            let state = bstate_rc.get();
                            if state == BlockState::CollectingOutput
                                || state == BlockState::AltScreen
                            {
                                continue;
                            }
                            let background_output = if state == BlockState::AwaitingCommand {
                                take_background_output(&background_output_rc)
                            } else {
                                None
                            };
                            let is_background = background_output.is_some();
                            // Finalize the previous command (deferred from CommandEnd),
                            // or turn commandless async output into a first-class block.
                            if state == BlockState::PostCommand || is_background {
                                // The VTE-text capture taken at CommandStart is
                                // authoritative — it reflects what was on screen
                                // when the user pressed Enter. Fall back to the
                                // keystroke shadow only if the VTE read came back
                                // empty (which would indicate the prompt-end
                                // anchor never captured a valid cursor position).
                                let mut cmd = if is_background {
                                    String::new()
                                } else {
                                    let vte_cmd = vte_typed_cmd_rc.borrow().trim().to_string();
                                    if !vte_cmd.is_empty() {
                                        vte_cmd
                                    } else {
                                        typed_cmd_rc.borrow().trim().to_string()
                                    }
                                };

                                if cmd.is_empty() && !is_background {
                                    // Never silently discard a command lifecycle. The
                                    // VTE range can be empty during an echo/feed race,
                                    // and line-editor control sequences do not always
                                    // populate the printable keystroke shadow. Keep a
                                    // visible diagnostic card whenever input activity
                                    // or actual output proves that something ran.
                                    let output_visible = background_output_has_visible_text(
                                        active_rc.borrow().output_text().as_bytes(),
                                    );
                                    if pty_synced_rc.get() || output_visible {
                                        log::warn!(
                                            "finished command text was unavailable; preserving block with placeholder"
                                        );
                                        cmd = "(command capture unavailable)".to_string();
                                    } else {
                                        // A genuinely empty submission with no output
                                        // is not useful history; reset for the prompt.
                                        let preserve =
                                            config_for_cb.borrow().preserve_live_scrollback;
                                        active_rc.borrow().reset_active(preserve);
                                        // Match jterm1's reset_active: half-uploaded
                                        // kitty chunks and undisplayed images do not
                                        // survive into the next command.
                                        kitty_assembler_rc.borrow_mut().reset();
                                        kitty_pending_images_rc.borrow_mut().clear();
                                        kitty_pending_bytes_rc.set(0);
                                        bstate_rc.set(BlockState::CollectingPrompt);
                                        prompt_buf_rc.borrow_mut().clear();
                                        scroll_debouncer.mark_dirty(&block_scroll_rc);
                                        continue;
                                    }
                                }

                                let prompt = if is_background {
                                    String::new()
                                } else {
                                    prompt_display_rc.borrow().clone()
                                };

                                // The raw bytes already carry CRLF — the PTY's
                                // ONLCR turns `\n` into `\r\n` on the master side
                                // before we ever see them — and the finished VTE
                                // handles in-line CR overwrites natively, just
                                // like the live VTE did while the command ran. So
                                // we feed the captured bytes verbatim, with no
                                // reconstruction pass.
                                let output_with_ansi = background_output
                                    .unwrap_or_else(|| active_rc.borrow().output_text());

                                let output_plain = strip_ansi(&output_with_ansi);

                                let truncation_limit =
                                    config_for_cb.borrow().truncation_threshold_lines as usize;
                                let (_output_trimmed, line_count) =
                                    truncate_plain_output_for_height(
                                        &output_plain,
                                        truncation_limit,
                                    );
                                let cols_for_height = active_rc.borrow().grid_cols() as i64;
                                let estimated_height = estimated_finished_block_height_for_text(
                                    &config_for_cb.borrow(),
                                    &cmd,
                                    &output_plain,
                                    cols_for_height,
                                );

                                let start_time = if is_background {
                                    None
                                } else {
                                    block_start_time_for_cb.get()
                                };
                                let now = SystemTime::now();
                                let end_time_ms = now
                                    .duration_since(SystemTime::UNIX_EPOCH)
                                    .ok()
                                    .map(|d| d.as_millis() as u64);
                                let start_time_ms = start_time.and_then(|st| {
                                    st.duration_since(SystemTime::UNIX_EPOCH)
                                        .ok()
                                        .map(|d| d.as_millis() as u64)
                                });
                                let measured_duration_ms = start_time.and_then(|st| {
                                    now.duration_since(st).ok().map(|d| d.as_millis() as u64)
                                });

                                // Background output belongs to no command, so it
                                // carries no shell metadata either.
                                let command_meta = if is_background {
                                    PendingCommandMeta::default()
                                } else {
                                    std::mem::take(&mut *pending_command_meta_rc.borrow_mut())
                                };

                                // The shell timed the command itself; our timer
                                // starts when the mark was noticed, which is
                                // later and includes our own parse latency.
                                let duration_ms = command_meta.duration_ms.or(measured_duration_ms);

                                let block_cwd = command_meta.cwd.clone().or_else(|| {
                                    let cwd_str = current_cwd_for_cb.borrow().clone();
                                    if cwd_str.is_empty() {
                                        None
                                    } else {
                                        Some(cwd_str)
                                    }
                                });

                                // None = the shell reported no status. Kept
                                // distinct from Some(0) everywhere downstream, so
                                // an unknown outcome is never presented as success.
                                let exit_code = if is_background {
                                    None
                                } else {
                                    pending_exit_code_rc.take()
                                };

                                // Single id shared by the serializable BlockData and
                                // the GTK FinishedBlock so id-keyed lookups (export,
                                // delete) resolve in both lists.
                                let block_id = next_block_id();
                                // Capture cols now (live VTE is allocated by the time
                                // a command finishes) and store it on BlockData so
                                // session restore can recreate the finished VTE at
                                // the same width — preserving column-formatted output
                                // (ls, git log, etc.) instead of reflowing it.
                                let cols = active_rc.borrow().grid_cols() as i64;
                                let block_output = output_plain.trim().to_string();

                                // Correlate what this terminal actually rendered
                                // with jsh's own execution record. jsh owns the
                                // command/cwd/exit/duration events; the id it put
                                // on the OSC 133 mark is the only key that can
                                // attach our captured output to them.
                                if let Some(id) = command_meta.id.clone() {
                                    submit_captured_output_to_journal(id, &block_output);
                                }

                                let block_data = BlockData {
                                    id: block_id,
                                    prompt: prompt.clone(),
                                    cmd: cmd.clone(),
                                    cmd_markup: None,
                                    output: block_output,
                                    exit_code,
                                    estimated_height,
                                    line_count,
                                    start_time_ms,
                                    end_time_ms,
                                    duration_ms,
                                    cwd: block_cwd.clone(),
                                    cols: cols.clamp(1, u16::MAX as i64) as u16,
                                };

                                block_data_for_cb.borrow_mut().push_back(block_data);

                                // Drain the kitty-graphics images decoded during
                                // this command so the finished block mounts them
                                // below its text output. Images are display-only:
                                // BlockData/history stay text-only, so a restored
                                // session simply omits them.
                                let kitty_images: Vec<gtk4::gdk::Texture> =
                                    kitty_pending_images_rc.borrow_mut().drain(..).collect();
                                kitty_pending_bytes_rc.set(0);

                                let recycled = widget_pool_for_cb.borrow_mut().acquire();
                                let finished = FinishedBlock::new_with_pool(
                                    block_id,
                                    &prompt,
                                    &cmd,
                                    None,
                                    &output_with_ansi,
                                    exit_code,
                                    &config_for_cb.borrow(),
                                    duration_ms,
                                    end_time_ms,
                                    block_cwd.as_deref(),
                                    cols,
                                    &kitty_images,
                                    recycled,
                                );
                                // A block finishing after an app recolored the
                                // terminal must not pop in with static theme
                                // colors next to the recolored live VTE.
                                apply_dynamic_colors_to_finished(
                                    &finished,
                                    dynamic_colors_rc.get(),
                                );
                                finished.widget().insert_before(
                                    &block_list_rc,
                                    Some(active_rc.borrow().widget()),
                                );

                                let was_user_scrolled = scroll_debouncer.user_scrolled_up.get();

                                // If the user is reading history (scrolled up), this
                                // freshly-finished block is "unread": bump the FAB badge
                                // so they can see work completed below and jump to it.
                                if was_user_scrolled {
                                    unread_count_rc.set(unread_count_rc.get().saturating_add(1));
                                    set_jump_fab_label(&jump_fab, unread_count_rc.get());
                                    jump_fab.set_visible(true);
                                }

                                let max_blocks = config_for_cb.borrow().max_visible_blocks as usize;
                                let finished_clone = finished.clone();
                                let finished_widget = finished_clone.widget().clone();

                                finished_clone.connect_actions(
                                    &active_vte,
                                    &pty_for_init,
                                    &pty_synced_rc,
                                    &active_rc,
                                    &typed_cmd_rc,
                                    &bstate_rc,
                                    &bracketed_paste_rc,
                                );
                                finished_clone.connect_scroll_forwarding(&block_scroll_rc);

                                finished_blocks_for_cb.borrow_mut().push(finished);

                                if !is_background {
                                    let output_sample = sample_output_for_event(&output_plain);
                                    for cb in block_finished_cbs.borrow().iter() {
                                        cb(
                                            cmd.clone(),
                                            exit_code,
                                            output_sample.clone(),
                                            duration_ms,
                                        );
                                    }
                                }

                                {
                                    let cfg = config_for_cb.borrow();
                                    if !is_background && cfg.notify_long_blocks {
                                        if let Some(ms) = duration_ms {
                                            if ms >= cfg.notify_long_block_threshold_ms {
                                                notify_long_block(&cmd, exit_code, ms);
                                            }
                                        }
                                    }
                                    // Re-probe git state — the command that just
                                    // finished may have changed branch/dirty/upstream.
                                    if cfg.show_repo_strip {
                                        let cwd = current_cwd_for_cb.borrow().clone();
                                        refresh_repo_strip(&repo_strip, &cwd);
                                    }
                                }

                                // Right-click context menu.
                                let finished_blocks_for_menu =
                                    Rc::downgrade(&finished_blocks_for_cb);
                                let block_list_for_menu = block_list_rc.downgrade();
                                let vte_for_copy = active_vte.downgrade();
                                let pty_for_rerun_menu = pty_for_init.clone();
                                let pty_synced_for_rerun_menu = pty_synced_rc.clone();
                                let active_for_rerun_menu = Rc::downgrade(&active_rc);
                                let bstate_for_rerun_menu = bstate_rc.clone();
                                let bracketed_paste_for_menu = bracketed_paste_rc.clone();
                                let typed_cmd_for_rerun_menu = typed_cmd_rc.clone();
                                let selected_ids_for_menu = selected_block_ids_rc.clone();
                                let selected_for_menu = selected_block_id_rc.clone();
                                let anchor_for_menu = selection_anchor_id_rc.clone();
                                let bookmarks_for_menu = bookmarks_for_cb.clone();
                                let visible_for_menu = visible_indices_rc.clone();
                                let block_id = finished_clone.id;

                                let right_click = gtk4::GestureClick::new();
                                right_click.set_button(3);

                                let finished_widget_for_menu = finished_widget.downgrade();
                                let long_output_for_menu = finished_clone.long_output;
                                let block_data_for_export = block_data_for_cb.clone();
                                let block_scroll_for_menu = block_scroll_rc.downgrade();
                                right_click.connect_pressed(move |gesture, _n_press, x, y| {
                                    let Some(finished_blocks) = finished_blocks_for_menu.upgrade()
                                    else {
                                        return;
                                    };
                                    let Some(vte_for_copy) = vte_for_copy.upgrade() else {
                                        return;
                                    };
                                    let Some(finished_widget) = finished_widget_for_menu.upgrade()
                                    else {
                                        return;
                                    };
                                    gesture.set_state(gtk4::EventSequenceState::Claimed);
                                    {
                                        let finished = finished_blocks.borrow();
                                        clear_vte_text_selections(&finished, &vte_for_copy);
                                        activate_finished_block_selection(
                                            &finished,
                                            &selected_ids_for_menu,
                                            &selected_for_menu,
                                            &anchor_for_menu,
                                            block_id,
                                        );
                                    }

                                    let popover = gtk4::Popover::new();
                                    popover.set_parent(&finished_widget);
                                    popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(
                                        x as i32, y as i32, 1, 1,
                                    )));
                                    popover.set_has_arrow(false);

                                    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
                                    vbox.add_css_class("menu");

                                    let make_item = |label: &str| -> gtk4::Button {
                                        let btn = gtk4::Button::with_label(label);
                                        btn.set_has_frame(false);
                                        btn.set_halign(gtk4::Align::Fill);
                                        if let Some(child) = btn.child() {
                                            child.set_halign(gtk4::Align::Start);
                                        }
                                        btn.add_css_class("flat");
                                        btn
                                    };

                                    let selected_count = selected_ids_for_menu.borrow().len();
                                    let has_selected_commands = {
                                        let selected = selected_ids_for_menu.borrow();
                                        block_data_for_export.borrow().iter().any(|block| {
                                            selected.contains(&block.id)
                                                && !block.cmd.trim().is_empty()
                                        })
                                    };

                                    if has_selected_commands {
                                        let item = make_item(if selected_count > 1 {
                                            "Copy Commands"
                                        } else {
                                            "Copy Command"
                                        });
                                        let popover_c = popover.downgrade();
                                        let block_data_for_copy = block_data_for_export.clone();
                                        let selected_ids_for_copy = selected_ids_for_menu.clone();
                                        let vte_for_action = vte_for_copy.downgrade();
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let Some(vte_for_action) = vte_for_action.upgrade()
                                            else {
                                                return;
                                            };
                                            let selected = selected_ids_for_copy.borrow();
                                            let blocks = block_data_for_copy.borrow();
                                            let text = selected_command_text(
                                                blocks
                                                    .iter()
                                                    .map(|block| (block.id, block.cmd.as_str())),
                                                &selected,
                                            );
                                            vte_for_action.clipboard().set_text(&text);
                                        });
                                        vbox.append(&item);
                                    }

                                    {
                                        let item = make_item(if selected_count > 1 {
                                            "Copy Outputs"
                                        } else {
                                            "Copy Output"
                                        });
                                        let popover_c = popover.downgrade();
                                        let block_data_for_copy = block_data_for_export.clone();
                                        let selected_ids_for_copy = selected_ids_for_menu.clone();
                                        let vte_for_action = vte_for_copy.downgrade();
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let Some(vte_for_action) = vte_for_action.upgrade()
                                            else {
                                                return;
                                            };
                                            let selected = selected_ids_for_copy.borrow();
                                            let blocks = block_data_for_copy.borrow();
                                            let text = selected_clipboard_text(
                                                blocks.iter(),
                                                &selected,
                                                |block| strip_ansi(&block.output),
                                            );
                                            vte_for_action.clipboard().set_text(&text);
                                        });
                                        vbox.append(&item);
                                    }

                                    {
                                        let item = make_item(if selected_count > 1 {
                                            "Copy Blocks"
                                        } else {
                                            "Copy Block"
                                        });
                                        let popover_c = popover.downgrade();
                                        let block_data_for_copy = block_data_for_export.clone();
                                        let selected_ids_for_copy = selected_ids_for_menu.clone();
                                        let vte_for_action = vte_for_copy.downgrade();
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let Some(vte_for_action) = vte_for_action.upgrade()
                                            else {
                                                return;
                                            };
                                            let selected = selected_ids_for_copy.borrow();
                                            let blocks = block_data_for_copy.borrow();
                                            let text = selected_clipboard_text(
                                                blocks.iter(),
                                                &selected,
                                                |block| {
                                                    block_clipboard_text(
                                                        &block.cmd,
                                                        &strip_ansi(&block.output),
                                                        false,
                                                    )
                                                },
                                            );
                                            vte_for_action.clipboard().set_text(&text);
                                        });
                                        vbox.append(&item);
                                    }

                                    if has_selected_commands {
                                        let item = make_item(if selected_count > 1 {
                                            "Insert Commands at Prompt"
                                        } else {
                                            "Insert Command at Prompt"
                                        });
                                        let popover_c = popover.downgrade();
                                        let finished_for_rerun = finished_blocks_for_menu.clone();
                                        let selected_ids_for_rerun = selected_ids_for_menu.clone();
                                        let selected_for_rerun = selected_for_menu.clone();
                                        let anchor_for_rerun = anchor_for_menu.clone();
                                        let pty_for_action = pty_for_rerun_menu.clone();
                                        let pty_synced_for_action =
                                            pty_synced_for_rerun_menu.clone();
                                        let bracketed_for_action = bracketed_paste_for_menu.clone();
                                        let typed_cmd_for_action = typed_cmd_for_rerun_menu.clone();
                                        let bstate_for_action = bstate_for_rerun_menu.clone();
                                        let active_for_action = active_for_rerun_menu.clone();
                                        item.set_sensitive(command_recall_available(
                                            bstate_for_rerun_menu.get(),
                                        ));
                                        item.set_tooltip_text(Some(
                                            "Available when the shell prompt is ready",
                                        ));
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let Some(finished_for_rerun) =
                                                finished_for_rerun.upgrade()
                                            else {
                                                return;
                                            };
                                            let finished = finished_for_rerun.borrow();
                                            let recalled = {
                                                let selected = selected_ids_for_rerun.borrow();
                                                recall_selected_commands_at_prompt(
                                                    &pty_for_action,
                                                    &pty_synced_for_action,
                                                    &typed_cmd_for_action,
                                                    bstate_for_action.get(),
                                                    &finished,
                                                    &selected,
                                                    bracketed_for_action.get(),
                                                )
                                            };
                                            if recalled {
                                                clear_finished_block_selection(
                                                    &finished,
                                                    &selected_ids_for_rerun,
                                                    &selected_for_rerun,
                                                    &anchor_for_rerun,
                                                );
                                                if let Some(active_for_action) =
                                                    active_for_action.upgrade()
                                                {
                                                    active_for_action.borrow().grab_focus();
                                                }
                                            }
                                        });
                                        vbox.append(&item);
                                    }

                                    {
                                        let item = make_item("Scroll to Top of Block");
                                        let popover_c = popover.downgrade();
                                        let finished = finished_blocks_for_menu.clone();
                                        let scroll = block_scroll_for_menu.clone();
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let (Some(finished), Some(scroll)) =
                                                (finished.upgrade(), scroll.upgrade())
                                            else {
                                                return;
                                            };
                                            let finished = finished.borrow();
                                            if let Some(block) =
                                                finished.iter().find(|b| b.id == block_id)
                                            {
                                                block.scroll_to_edge(&scroll, false);
                                            }
                                        });
                                        vbox.append(&item);
                                    }
                                    if long_output_for_menu {
                                        let item = make_item("Jump to Bottom of Block");
                                        let popover_c = popover.downgrade();
                                        let finished = finished_blocks_for_menu.clone();
                                        let scroll = block_scroll_for_menu.clone();
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let (Some(finished), Some(scroll)) =
                                                (finished.upgrade(), scroll.upgrade())
                                            else {
                                                return;
                                            };
                                            let finished = finished.borrow();
                                            if let Some(block) =
                                                finished.iter().find(|b| b.id == block_id)
                                            {
                                                block.scroll_to_edge(&scroll, true);
                                            }
                                        });
                                        vbox.append(&item);
                                    }
                                    {
                                        let item = make_item("Toggle Output Filter");
                                        let popover_c = popover.downgrade();
                                        let finished = finished_blocks_for_menu.clone();
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let Some(finished) = finished.upgrade() else {
                                                return;
                                            };
                                            let finished = finished.borrow();
                                            if let Some(block) =
                                                finished.iter().find(|b| b.id == block_id)
                                            {
                                                (block.toggle_filter)();
                                            }
                                        });
                                        vbox.append(&item);
                                    }
                                    {
                                        let bookmarked =
                                            bookmarks_for_menu.borrow().contains(&block_id);
                                        let item = make_item(if bookmarked {
                                            "Remove Bookmark"
                                        } else {
                                            "Bookmark Block"
                                        });
                                        let popover_c = popover.downgrade();
                                        let finished = finished_blocks_for_menu.clone();
                                        let bookmarks = bookmarks_for_menu.clone();
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let Some(finished) = finished.upgrade() else {
                                                return;
                                            };
                                            let finished = finished.borrow();
                                            let Some(block) =
                                                finished.iter().find(|b| b.id == block_id)
                                            else {
                                                return;
                                            };
                                            let mut marks = bookmarks.borrow_mut();
                                            let now_bookmarked = if marks.remove(&block_id) {
                                                false
                                            } else {
                                                marks.insert(block_id);
                                                true
                                            };
                                            block.bookmark_star.set_visible(now_bookmarked);
                                            if now_bookmarked {
                                                block.widget().add_css_class("block-bookmarked");
                                            } else {
                                                block.widget().remove_css_class("block-bookmarked");
                                            }
                                        });
                                        vbox.append(&item);
                                    }

                                    let separator =
                                        gtk4::Separator::new(gtk4::Orientation::Horizontal);
                                    vbox.append(&separator);

                                    {
                                        let item = make_item("Export as JSON");
                                        let popover_c = popover.downgrade();
                                        let block_data_for_json = block_data_for_export.clone();
                                        let vte_for_json = vte_for_copy.downgrade();
                                        let block_id_json = block_id;
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let Some(vte_for_json) = vte_for_json.upgrade() else {
                                                return;
                                            };
                                            let blocks = block_data_for_json.borrow();
                                            if let Some(block) =
                                                blocks.iter().find(|b| b.id == block_id_json)
                                            {
                                                let json = block.to_json();
                                                vte_for_json.clipboard().set_text(&json);
                                            }
                                        });
                                        vbox.append(&item);
                                    }

                                    {
                                        let item = make_item("Export as Markdown");
                                        let popover_c = popover.downgrade();
                                        let block_data_for_md = block_data_for_export.clone();
                                        let vte_for_md = vte_for_copy.downgrade();
                                        let block_id_md = block_id;
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let Some(vte_for_md) = vte_for_md.upgrade() else {
                                                return;
                                            };
                                            let blocks = block_data_for_md.borrow();
                                            if let Some(block) =
                                                blocks.iter().find(|b| b.id == block_id_md)
                                            {
                                                let markdown = block.to_markdown();
                                                vte_for_md.clipboard().set_text(&markdown);
                                            }
                                        });
                                        vbox.append(&item);
                                    }

                                    {
                                        let item = make_item("Delete Block");
                                        let popover_c = popover.downgrade();
                                        let finished_blocks_for_delete =
                                            finished_blocks_for_menu.clone();
                                        let block_list_for_delete = block_list_for_menu.clone();
                                        let block_data_for_delete = block_data_for_export.clone();
                                        let selected_ids_for_delete = selected_ids_for_menu.clone();
                                        let selected_for_delete = selected_for_menu.clone();
                                        let anchor_for_delete = anchor_for_menu.clone();
                                        let bookmarks_for_delete = bookmarks_for_menu.clone();
                                        let visible_for_delete = visible_for_menu.clone();
                                        let block_id_del = block_id;
                                        item.connect_clicked(move |_| {
                                            popdown_if_alive(&popover_c);
                                            let (
                                                Some(finished_blocks_for_delete),
                                                Some(block_list_for_delete),
                                            ) = (
                                                finished_blocks_for_delete.upgrade(),
                                                block_list_for_delete.upgrade(),
                                            )
                                            else {
                                                return;
                                            };
                                            let _ = remove_finished_block(
                                                block_id_del,
                                                &finished_blocks_for_delete,
                                                &block_data_for_delete,
                                                &block_list_for_delete,
                                                BlockSelectionRefs {
                                                    ids: &selected_ids_for_delete,
                                                    active: &selected_for_delete,
                                                    anchor: &anchor_for_delete,
                                                },
                                                &bookmarks_for_delete,
                                                &visible_for_delete,
                                            );
                                        });
                                        vbox.append(&item);
                                    }

                                    popover.set_child(Some(&vbox));
                                    popover.connect_closed(move |p| {
                                        p.unparent();
                                    });
                                    popover.popup();
                                });
                                finished_widget.add_controller(right_click);

                                install_finished_block_selection(
                                    &finished_clone,
                                    &active_rc,
                                    &finished_blocks_for_cb,
                                    &selected_block_ids_rc,
                                    &selected_block_id_rc,
                                    &selection_anchor_id_rc,
                                );

                                while finished_blocks_for_cb.borrow().len() > max_blocks {
                                    let oldest = finished_blocks_for_cb.borrow_mut().remove(0);
                                    remove_finished_block_from_selection(
                                        &finished_blocks_for_cb.borrow(),
                                        &selected_block_ids_rc,
                                        &selected_block_id_rc,
                                        &selection_anchor_id_rc,
                                        oldest.id,
                                    );
                                    bookmarks_for_cb.borrow_mut().remove(&oldest.id);
                                    {
                                        let mut visible = visible_indices_rc.borrow_mut();
                                        let shifted = visible
                                            .iter()
                                            .filter_map(|&i| i.checked_sub(1))
                                            .collect();
                                        *visible = shifted;
                                    }
                                    let widget_to_release = oldest.widget().clone();
                                    block_list_rc.remove(&widget_to_release);
                                    widget_pool_for_cb.borrow_mut().release(widget_to_release);
                                }

                                while block_data_for_cb.borrow().len() > max_blocks {
                                    block_data_for_cb.borrow_mut().pop_front();
                                }

                                let preserve = config_for_cb.borrow().preserve_live_scrollback;
                                active_rc.borrow().reset_active(preserve);
                                // Drop any half-uploaded kitty chunks so they can't
                                // leak into the next command (the finalize above
                                // already drained every completed image).
                                kitty_assembler_rc.borrow_mut().reset();
                                kitty_pending_images_rc.borrow_mut().clear();
                                kitty_pending_bytes_rc.set(0);
                                if !was_user_scrolled {
                                    scroll_debouncer.reset_scroll_lock();
                                    scroll_debouncer.pin_to_bottom_deferred(&block_scroll_rc);
                                }
                            }
                            bstate_rc.set(BlockState::CollectingPrompt);
                            prompt_buf_rc.borrow_mut().clear();
                            // Reassert the stable viewport grid before the shell
                            // renders the next prompt.
                            sync_active_to_pty(
                                &layout_active_surface,
                                &active_vte,
                                &block_scroll_rc,
                                &pty_for_init,
                            );
                            scroll_debouncer.mark_dirty(&block_scroll_rc);
                        }

                        ParserEvent::PromptEnd => {
                            if bstate_rc.get() != BlockState::CollectingPrompt {
                                continue;
                            }
                            // Capture the rendered prompt (last non-empty line) for the
                            // finished block / export.
                            let prompt_line = {
                                let pb = prompt_buf_rc.borrow();
                                strip_ansi(&pb)
                                    .lines()
                                    .rev()
                                    .find(|l| !l.trim().is_empty())
                                    .unwrap_or("")
                                    .trim()
                                    .to_string()
                            };
                            *prompt_display_rc.borrow_mut() = prompt_line;
                            prompt_buf_rc.borrow_mut().clear();
                            typed_cmd_rc.borrow_mut().clear();
                            vte_typed_cmd_rc.borrow_mut().clear();
                            external_submission_rc.borrow_mut().take();
                            background_output_rc.borrow_mut().clear();
                            idle_input_dirty_rc.set(false);
                            // Snapshot the live VTE cursor at the moment the
                            // prompt finishes drawing — this is where the user's
                            // command starts. CommandStart will read text from
                            // here to the cursor's then-position to recover the
                            // command as it really appeared on screen.
                            let (col, row) = active_vte.cursor_position();
                            prompt_end_pos_rc.set((col, row));
                            pty_synced_rc.set(false);
                            bstate_rc.set(BlockState::AwaitingCommand);
                            layout_active_surface();
                            let active_for_focus = active_rc.clone();
                            glib::idle_add_local_once(move || {
                                active_for_focus.borrow().grab_focus();
                            });

                            // Feed next initial command if any. Seed the same
                            // fallback state as interactive input before writing, so
                            // a fast command cannot outrun command capture.
                            if let Some(cmd) = init_cmds_queue_for_cb.borrow_mut().pop_front() {
                                let mut typed = typed_cmd_rc.borrow_mut();
                                typed.clear();
                                append_typed_command_shadow(&mut typed, &cmd);
                                drop(typed);
                                *external_submission_rc.borrow_mut() = Some(cmd.clone());
                                idle_input_dirty_rc.set(true);
                                pty_synced_rc.set(true);
                                let text = format!("{}\r", cmd);
                                if let Err(error) = pty_for_init.write_bytes(text.as_bytes()) {
                                    // The shadow was armed before enqueue so a
                                    // fast child cannot outrun capture. Roll it
                                    // back when bounded admission rejects the
                                    // whole command.
                                    typed_cmd_rc.borrow_mut().clear();
                                    external_submission_rc.borrow_mut().take();
                                    idle_input_dirty_rc.set(false);
                                    pty_synced_rc.set(false);
                                    pty_for_init.report_write_error(
                                        "could not queue initial command",
                                        error,
                                    );
                                }
                            }

                            scroll_debouncer.reset_scroll_lock();
                            scroll_debouncer.mark_dirty(&block_scroll_rc);
                        }

                        ParserEvent::CommandStart(meta) => {
                            ftcs_seen_rc.set(true);
                            let state = bstate_rc.get();
                            if state == BlockState::CollectingOutput
                                || state == BlockState::AltScreen
                            {
                                osc133_depth_rc.set(osc133_depth_rc.get() + 1);
                                continue;
                            }
                            if state != BlockState::AwaitingCommand {
                                continue;
                            }
                            osc133_depth_rc.set(0);
                            *pending_command_meta_rc.borrow_mut() =
                                PendingCommandMeta::from_command_start(meta);
                            // A command start without an intervening PromptStart is
                            // an ambiguous shell-integration edge. Keep those bytes
                            // visible in the live VTE but do not merge them into the
                            // command's output block.
                            background_output_rc.borrow_mut().clear();
                            active_rc.borrow().reset_output_buffer();
                            block_start_time_for_cb.set(Some(SystemTime::now()));
                            // Read the typed command directly off the live VTE,
                            // not from a shadow keystroke buffer. The VTE shows
                            // what the user actually saw — including history
                            // recalls and jsh autosuggestion accepts — so what we
                            // capture here is faithful to the run. Range goes
                            // from the cursor position captured at PromptEnd to
                            // the current cursor position (right before the
                            // shell echoes a newline and starts the command).
                            let (cmd_end_col, cmd_end_row) = active_vte.cursor_position();
                            let (start_col, start_row) = prompt_end_pos_rc.get();
                            let captured = if command_capture_range_is_bounded(
                                start_row,
                                cmd_end_row,
                                active_vte.column_count(),
                            ) {
                                active_vte
                                    .text_range_format(
                                        vte4::Format::Text,
                                        start_row,
                                        start_col,
                                        cmd_end_row,
                                        cmd_end_col,
                                    )
                                    .0
                                    .map(|gs| bounded_command_text(&gs))
                                    .unwrap_or_default()
                            } else {
                                TRUNCATED_COMMAND_PLACEHOLDER.to_string()
                            };
                            let prompt_display = prompt_display_rc.borrow().clone();
                            let typed_shadow = typed_cmd_rc.borrow().clone();
                            let external_submission = external_submission_rc.borrow_mut().take();
                            let submitted_command = resolve_command_for_block(
                                meta,
                                &resolve_submitted_command(
                                    &captured,
                                    &prompt_display,
                                    &typed_shadow,
                                    external_submission.as_deref(),
                                ),
                            );
                            *vte_typed_cmd_rc.borrow_mut() = submitted_command.clone();
                            *running_cmd_rc.borrow_mut() = submitted_command;
                            cmd_running_rc.set(true);
                            bstate_rc.set(BlockState::CollectingOutput);
                            typed_cmd_rc.borrow_mut().clear();
                            // Match jterm1's block-mode runtime model: keep the
                            // active VTE as the live surface while the command
                            // runs, then snapshot it into a finished block on the
                            // next prompt. Interactive CLIs such as Codex rely on
                            // VTE applying cursor positioning/redraws directly.
                            sync_active_to_pty(
                                &layout_active_surface,
                                &active_vte,
                                &block_scroll_rc,
                                &pty_for_init,
                            );
                            scroll_debouncer.mark_dirty(&block_scroll_rc);
                        }

                        ParserEvent::CommandEnd { exit, meta } => {
                            let state = bstate_rc.get();
                            if state != BlockState::CollectingOutput
                                && state != BlockState::AltScreen
                            {
                                continue;
                            }
                            if osc133_depth_rc.get() > 0 {
                                osc133_depth_rc.set(osc133_depth_rc.get() - 1);
                                continue;
                            }
                            // Safety net (Warp parity): if the alt-screen app
                            // crashed or exited without rmcup, force the UI back
                            // to the block list so the next prompt is usable.
                            if state == BlockState::AltScreen {
                                let mode = active_alt_screen_mode_rc.replace(None).unwrap_or(1049);
                                let leave = format!("\x1b[?{mode}l");
                                active_vte.feed(leave.as_bytes());
                                exit_fullscreen(
                                    &finished_blocks_for_cb,
                                    &visible_indices_rc,
                                    &fullscreen_rc,
                                );
                                {
                                    let config = config_for_cb.borrow();
                                    let cwd = current_cwd_for_cb.borrow();
                                    exit_alt_screen_chrome(
                                        &active_rc,
                                        &sticky_bar,
                                        &jump_fab,
                                        &repo_strip,
                                        &config,
                                        cwd.as_str(),
                                        scroll_debouncer.user_scrolled_up.get(),
                                        unread_count_rc.get(),
                                    );
                                }
                                layout_active_surface();
                            }
                            pending_exit_code_rc.set(*exit);
                            pending_command_meta_rc.borrow_mut().merge_command_end(meta);
                            cmd_running_rc.set(false);
                            bstate_rc.set(BlockState::PostCommand);
                            scroll_debouncer.mark_dirty(&block_scroll_rc);
                        }

                        ParserEvent::AltScreenEnter(mode) => {
                            let from_state = bstate_rc.get();
                            if from_state != BlockState::CollectingOutput
                                && from_state != BlockState::AwaitingCommand
                            {
                                continue;
                            }
                            prev_state_rc.set(from_state);
                            bstate_rc.set(BlockState::AltScreen);
                            active_alt_screen_mode_rc.set(Some(*mode));
                            enter_alt_screen_chrome(
                                &active_rc,
                                &sticky_bar,
                                &jump_fab,
                                &repo_strip,
                            );
                            // Hand the viewport to the alt-screen app: hide finished
                            // blocks so the live VTE fills the scroll area.
                            enter_fullscreen(
                                &finished_blocks_for_cb,
                                &visible_indices_rc,
                                &fullscreen_rc,
                            );
                            // Grow the live VTE to the full viewport before the
                            // app draws (see sync_active_to_pty doc).
                            sync_active_to_pty(
                                &layout_active_surface,
                                &active_vte,
                                &block_scroll_rc,
                                &pty_for_init,
                            );
                            let enter = format!("\x1b[?{mode}h");
                            active_vte.feed(enter.as_bytes());
                        }

                        ParserEvent::AltScreenLeave(mode) => {
                            if bstate_rc.get() != BlockState::AltScreen {
                                continue;
                            }
                            // Warp parity: alt-screen content is ephemeral and is
                            // NOT merged into the block. The active block keeps
                            // just the command name + exit code.
                            active_alt_screen_mode_rc.set(None);
                            let leave = format!("\x1b[?{mode}l");
                            active_vte.feed(leave.as_bytes());
                            exit_fullscreen(
                                &finished_blocks_for_cb,
                                &visible_indices_rc,
                                &fullscreen_rc,
                            );
                            {
                                let config = config_for_cb.borrow();
                                let cwd = current_cwd_for_cb.borrow();
                                exit_alt_screen_chrome(
                                    &active_rc,
                                    &sticky_bar,
                                    &jump_fab,
                                    &repo_strip,
                                    &config,
                                    cwd.as_str(),
                                    scroll_debouncer.user_scrolled_up.get(),
                                    unread_count_rc.get(),
                                );
                            }
                            osc133_depth_rc.set(0);
                            bstate_rc.set(prev_state_rc.get());
                            // The primary and alternate screens share the same
                            // viewport-sized grid, just like regular VTE mode.
                            sync_active_to_pty(
                                &layout_active_surface,
                                &active_vte,
                                &block_scroll_rc,
                                &pty_for_init,
                            );
                            let active_for_idle = active_rc.clone();
                            glib::idle_add_local_once(move || {
                                active_for_idle.borrow().grab_focus();
                            });
                        }

                        ParserEvent::ClipboardSet(text) => {
                            if config_for_cb.borrow().allow_remote_clipboard_write {
                                if let Some(display) = gtk4::gdk::Display::default() {
                                    let clipboard = display.clipboard();
                                    clipboard.set_text(text);
                                }
                            }
                        }

                        ParserEvent::ClipboardQuery => {
                            if let Err(error) = pty_for_init.write_bytes(b"\x1b]52;c;\x1b\\") {
                                pty_for_init.report_write_error(
                                    "could not queue clipboard-query reply",
                                    error,
                                );
                            }
                        }

                        ParserEvent::ColorQuery(kind) => {
                            let reply = build_color_query_reply(
                                &config_for_cb.borrow(),
                                dynamic_colors_rc.get(),
                                *kind,
                            );
                            if let Err(error) = pty_for_init.write_bytes(reply.as_bytes()) {
                                pty_for_init
                                    .report_write_error("could not queue color-query reply", error);
                            }
                        }

                        ParserEvent::ColorSet { kind, spec } => {
                            // OSC 10/11/12 with a value: the raw bytes already
                            // passed through to the live VTE (native recolor);
                            // only the tracker updates here so the next
                            // ColorQuery reports the live color, not the theme.
                            let mut dynamic = dynamic_colors_rc.get();
                            dynamic.set(*kind, spec);
                            dynamic_colors_rc.set(dynamic);
                        }

                        ParserEvent::ColorReset(kind) => {
                            // OSC 110/111/112: bytes also passed through;
                            // queries fall back to the static theme again.
                            let mut dynamic = dynamic_colors_rc.get();
                            dynamic.reset(*kind);
                            dynamic_colors_rc.set(dynamic);
                        }

                        ParserEvent::KeyboardProtocolQuery(query) => {
                            let (col, row) = active_vte.cursor_position();
                            let reply = build_keyboard_query_reply(*query, col, row);
                            if let Err(error) = pty_for_init.write_bytes(reply.as_bytes()) {
                                pty_for_init.report_write_error(
                                    "could not queue keyboard-query reply",
                                    error,
                                );
                            }
                        }

                        ParserEvent::RemoteSessionId(id) => {
                            if crate::review_input::valid_jsh_id(id) {
                                for cb in remote_session_cbs.borrow().iter() {
                                    cb(id);
                                }
                            }
                        }

                        ParserEvent::Notification { title, body } => {
                            // Desktop notification requested via OSC 9 / OSC 777.
                            // The parser already stripped controls and bounded the
                            // text; this side only enforces the app-wide rate limit
                            // (at most one per batch, dropping the rest).
                            let now = std::time::Instant::now();
                            let allowed = LAST_NOTIFICATION_AT.with(|last| {
                                let ok = notification_allowed(last.get(), now);
                                if ok {
                                    last.set(Some(now));
                                }
                                ok
                            });
                            if allowed {
                                let title = title.as_deref().map(|title| {
                                    crate::review_input::safe_inline_display(title, 1_024)
                                });
                                let body =
                                    crate::review_input::safe_inline_display(body, 4 * 1_024);
                                if !body.trim().is_empty() {
                                    crate::notify::app_notification(title.as_deref(), &body);
                                }
                            }
                        }

                        ParserEvent::ApcSequence(payload) => {
                            // APC G — Kitty graphics. libvte has no APC graphics
                            // handler, so forwarding these bytes to the live VTE
                            // (the previous behaviour) silently dropped every
                            // inline image. Decode them here instead, regardless
                            // of block state — tools like `kitten icat` emit them
                            // at the shell prompt (main screen), not only inside
                            // alt-screen apps. Completed textures accumulate
                            // against the running command and are mounted on its
                            // finished block. Non-G APC payloads keep the silent
                            // consume today's libvte would apply.
                            if payload.first() == Some(&b'G') {
                                let outcome = kitty_assembler_rc.borrow_mut().feed(payload);
                                // Answer before consuming the outcome: clients
                                // like `kitten icat` block on the `i=`-keyed
                                // OK/error reply (jterm2's responder semantics;
                                // jterm1 never answers).
                                if let Some(reply) = kitty_graphics::response_for(payload, &outcome)
                                {
                                    if let Err(error) = pty_for_init.write_bytes(&reply) {
                                        pty_for_init.report_write_error(
                                            "could not queue graphics-protocol reply",
                                            error,
                                        );
                                    }
                                }
                                if let kitty_graphics::Outcome::Complete(texture) = outcome {
                                    // Rough memory bound: width*height*4 (bytes
                                    // per RGBA pixel). Once the shared per-block
                                    // budget is exhausted, further images drop —
                                    // the transmission was still acknowledged
                                    // above, only the display is skipped.
                                    let approx = (texture.width() as usize)
                                        .saturating_mul(texture.height() as usize)
                                        .saturating_mul(4);
                                    let used = kitty_pending_bytes_rc.get();
                                    if used + approx <= kitty_graphics::MAX_PENDING_BYTES_PER_BLOCK
                                    {
                                        kitty_pending_bytes_rc.set(used + approx);
                                        kitty_pending_images_rc.borrow_mut().push(texture);
                                    } else {
                                        log::warn!(
                                            "kitty graphics: per-block image budget exhausted ({} + {} > {}), dropping",
                                            used,
                                            approx,
                                            kitty_graphics::MAX_PENDING_BYTES_PER_BLOCK
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            },
        ));

        // Parked chunks flow back through the pipeline on flush. Weak: the
        // strong owner is the reader callback below, whose lifetime is the
        // PTY's — after teardown a late grace-timer flush must not revive it.
        selection_feed_hold.set_flush({
            let process = Rc::downgrade(&process_chunk);
            move |bytes: Vec<u8>| {
                if let Some(process) = process.upgrade() {
                    (process.borrow_mut())(bytes);
                }
            }
        });

        let hold_for_reader = selection_feed_hold.clone();
        pty.start_reader(
            move |data: Vec<u8>| {
                if hold_for_reader.try_buffer(&data) {
                    return;
                }
                (process_chunk.borrow_mut())(data);
            },
            move |exit_code| {
                log::debug!("Shell exited with code {}", exit_code);
                for cb in exited_cbs.borrow().iter() {
                    cb(exit_code);
                }
            },
        );
    }
}

/// Lay out the live surface and push the full viewport grid to the PTY
/// synchronously. The visual surface may be compact while the user is typing,
/// but terminal geometry remains identical to regular VTE mode. Used at state
/// transitions where the child needs to see a correct winsize on its very first
/// read — `top` queries TIOCGWINSZ before painting, less/vim do the same.
/// Without the synchronous push the per-frame resize tick would catch up only
/// on the next frame, racing with the child.
fn sync_active_to_pty(
    layout_active_surface: &Rc<dyn Fn()>,
    vte: &Terminal,
    scroll: &ScrolledWindow,
    pty: &OwnedPty,
) {
    layout_active_surface();
    let (cols, rows) = pty_grid_size(vte, scroll);
    pty.resize(cols, rows);
}

fn pty_grid_size(vte: &Terminal, scroll: &ScrolledWindow) -> (u16, u16) {
    let cols = vte.column_count().max(1) as u16;
    let rows = viewport_rows_for(vte, scroll)
        .unwrap_or_else(|| vte.row_count().max(1))
        .clamp(1, u16::MAX as i64) as u16;
    (cols, rows)
}

fn viewport_rows_for(vte: &Terminal, scroll: &ScrolledWindow) -> Option<i64> {
    let cell_h = (vte.char_height() as i32).max(1);
    let page = scroll.vadjustment().page_size() as i32;
    if page <= 1 {
        return None;
    }
    // Normal active cards reserve margin/border/padding. Alt-screen mode removes
    // that chrome so vim/less/htop receive every row in the pane.
    let fullscreen = vte
        .ancestor(gtk4::Box::static_type())
        .and_then(|widget| widget.downcast::<gtk4::Box>().ok())
        .is_some_and(|holder| holder.has_css_class("block-fullscreen"));
    let chrome = if fullscreen {
        0
    } else {
        css::BLOCK_ACTIVE_VCHROME_PX
    };
    let usable = (page - chrome).max(cell_h);
    Some(((usable / cell_h).max(1)) as i64)
}

fn compute_viewport_state(
    block_data: &VecDeque<BlockData>,
    visible_top: i32,
    visible_bottom: i32,
) -> ViewportState {
    let mut y = 0;
    let mut first = None;
    let mut last = 0;
    let mut iter = block_data.iter().enumerate();

    while let Some((i, block)) = iter.next() {
        let block_top = y;
        let block_bottom = y + block.estimated_height;
        if first.is_none() && block_bottom > visible_top {
            first = Some(i);
        }
        if block_top < visible_bottom {
            last = i;
        }
        y = block_bottom;

        if first.is_some() && y >= visible_bottom {
            for (_, block) in iter {
                y += block.estimated_height;
            }
            break;
        }
    }

    ViewportState {
        first_visible: first.unwrap_or(0),
        last_visible: last,
        total_height: y,
    }
}

/// Convert GTK's scroll geometry into a usable block viewport.
///
/// Notebook pages temporarily report a zero-sized adjustment while they are
/// unmapped during tab switches. Treating that transient geometry as a real
/// viewport can produce `first_visible > last_visible` at an exact block
/// boundary, virtualizing every card and leaving only empty placeholders. Keep
/// the last valid visibility set until the page is mapped and allocated again.
fn viewport_state_for_scroll(
    block_data: &VecDeque<BlockData>,
    scroll_top: f64,
    viewport_height: f64,
    margin_pages: u32,
) -> Option<ViewportState> {
    if !scroll_top.is_finite() || !viewport_height.is_finite() || viewport_height < 1.0 {
        return None;
    }

    let scroll_top = scroll_top.max(0.0) as i32;
    let viewport_height = viewport_height as i32;
    if viewport_height <= 0 {
        return None;
    }
    let margin_pages = i32::try_from(margin_pages).unwrap_or(i32::MAX);
    let margin = viewport_height.saturating_mul(margin_pages);
    let visible_top = scroll_top.saturating_sub(margin).max(0);
    let visible_bottom = scroll_top
        .saturating_add(viewport_height)
        .saturating_add(margin);
    if visible_bottom <= visible_top {
        return None;
    }

    Some(compute_viewport_state(
        block_data,
        visible_top,
        visible_bottom,
    ))
}

/// `Adjustment::changed` covers every range mutation, including `upper`
/// changes caused by virtualizing a card. Visibility only needs another pass
/// when the viewport extent itself changed; treating an upper-only mutation as
/// a resize feeds the visibility side effect straight back into itself.
fn viewport_page_size_changed(last_page_size: &Cell<Option<f64>>, page_size: f64) -> bool {
    if !page_size.is_finite() {
        return false;
    }
    let changed = last_page_size
        .get()
        .is_none_or(|last| (last - page_size).abs() > 0.5);
    if changed {
        last_page_size.set(Some(page_size));
    }
    changed
}

fn visible_indices_for_viewport(vp: &ViewportState) -> std::collections::HashSet<usize> {
    let mut new_visible = std::collections::HashSet::new();
    for i in vp.first_visible..=vp.last_visible.min(vp.first_visible + 1000) {
        new_visible.insert(i);
    }
    new_visible
}

/// Visibility with hysteresis. Toggling a card's virtualization changes
/// document geometry; when the view is pinned at the bottom, GTK clamps the
/// scroll value to the shrunken `upper`, and that value change schedules
/// another visibility pass. Recomputed from the clamped value, a card sitting
/// exactly on the window boundary flips back, moving the geometry again — a
/// self-sustaining two-frame oscillation while the terminal is otherwise idle.
///
/// Cards inside the strict window always render; already-rendered cards keep
/// rendering until they leave a window one margin page looser. Sub-page scroll
/// jitter therefore can never toggle a card's state, and the feedback loop has
/// no edge to travel.
fn stable_visible_indices(
    strict: &ViewportState,
    loose: Option<&ViewportState>,
    current: &std::collections::HashSet<usize>,
) -> std::collections::HashSet<usize> {
    let mut next = visible_indices_for_viewport(strict);
    if let Some(loose) = loose {
        let keep = visible_indices_for_viewport(loose);
        next.extend(current.iter().copied().filter(|i| keep.contains(i)));
    }
    next
}

fn apply_visible_indices(
    finished: &[FinishedBlock],
    block_data: &mut VecDeque<BlockData>,
    visible: &mut std::collections::HashSet<usize>,
    new_visible: std::collections::HashSet<usize>,
) {
    for (i, block) in finished.iter().enumerate() {
        let should_render = new_visible.contains(&i);
        let height = block.set_virtualized(!should_render);
        if !should_render {
            if let Some(data) = block_data.get_mut(i) {
                data.estimated_height = height;
            }
        } else {
            // Keep the metadata document converged to real allocations for
            // rendered cards too. The font-metric estimate drifts from the
            // pixels GTK actually allocates, and the pixel→index mapping in
            // `compute_viewport_state` accumulates that drift — enough of it
            // moves the virtualization boundary onto cards that are still on
            // screen.
            let allocated = block.widget().height();
            if allocated > 1 {
                if let Some(data) = block_data.get_mut(i) {
                    data.estimated_height = allocated;
                }
            }
        }
    }
    *visible = new_visible;
}

/// Hand the viewport to an alt-screen app: hide every finished block so the live
/// VTE fills the scroll area like a normal full-screen terminal.
fn enter_fullscreen(
    finished: &Rc<RefCell<Vec<FinishedBlock>>>,
    visible_indices: &Rc<RefCell<std::collections::HashSet<usize>>>,
    fullscreen: &Rc<Cell<bool>>,
) {
    if fullscreen.replace(true) {
        return;
    }
    let finished = finished.borrow();
    let _visible = visible_indices.borrow();
    for block in finished.iter() {
        block.widget().set_visible(false);
    }
}

/// Restore the block list when the alt-screen app exits, re-applying virtual-scroll
/// visibility so only the previously-visible blocks reappear.
fn exit_fullscreen(
    finished: &Rc<RefCell<Vec<FinishedBlock>>>,
    visible_indices: &Rc<RefCell<std::collections::HashSet<usize>>>,
    fullscreen: &Rc<Cell<bool>>,
) {
    if !fullscreen.replace(false) {
        return;
    }
    let _visible = visible_indices.borrow();
    for block in finished.borrow().iter() {
        // The outer placeholder remains part of the history document; each card's
        // content remembers whether virtual scrolling had unmapped it.
        block.widget().set_visible(true);
    }
}

fn enter_alt_screen_chrome(
    active: &Rc<RefCell<ActiveBlock>>,
    sticky: &gtk4::Box,
    jump_fab: &gtk4::Button,
    repo_strip: &gtk4::Label,
) {
    active.borrow().widget().add_css_class("block-fullscreen");
    sticky.set_visible(false);
    jump_fab.set_visible(false);
    repo_strip.set_visible(false);
}

#[allow(clippy::too_many_arguments)]
fn exit_alt_screen_chrome(
    active: &Rc<RefCell<ActiveBlock>>,
    sticky: &gtk4::Box,
    jump_fab: &gtk4::Button,
    repo_strip: &gtk4::Label,
    config: &Config,
    cwd: &str,
    user_scrolled: bool,
    unread: u32,
) {
    active
        .borrow()
        .widget()
        .remove_css_class("block-fullscreen");
    sticky.set_visible(false);
    if user_scrolled {
        set_jump_fab_label(jump_fab, unread);
        jump_fab.set_visible(true);
    } else {
        jump_fab.set_visible(false);
    }
    if config.show_repo_strip {
        refresh_repo_strip(repo_strip, cwd);
    } else {
        repo_strip.set_visible(false);
    }
}

fn running_root_control_bytes(
    keyval: gtk4::gdk::Key,
    modifiers: gtk4::gdk::ModifierType,
) -> Option<&'static [u8]> {
    use gtk4::gdk::Key;

    let ctrl = modifiers.contains(gtk4::gdk::ModifierType::CONTROL_MASK);
    let alt = modifiers.contains(gtk4::gdk::ModifierType::ALT_MASK);
    if !ctrl || alt {
        return None;
    }

    if matches!(keyval, Key::c | Key::C) {
        Some(b"\x03")
    } else if matches!(keyval, Key::d | Key::D) {
        Some(b"\x04")
    } else {
        None
    }
}

/// Captures the handles the live-VTE key handler needs. With the VTE owning line
/// editing + IME natively (jterm1 model), this is reduced to a Capture-phase
/// navigation / copy-paste / block-selection handler; printable keys and editing
/// fall through to the VTE.
struct KeyCtx {
    pty_for_key: Rc<OwnedPty>,
    active_vte_for_key: glib::WeakRef<Terminal>,
    pty_synced_for_key: Rc<Cell<bool>>,
    bracketed_paste_for_key: Rc<Cell<bool>>,
    typed_cmd_for_key: Rc<RefCell<String>>,
    finished_blocks_for_key: Weak<RefCell<Vec<FinishedBlock>>>,
    block_data_for_key: Rc<RefCell<VecDeque<BlockData>>>,
    block_list_for_key: glib::WeakRef<gtk4::Box>,
    selected_block_ids_for_key: SelectedBlockIds,
    selected_block_id_for_key: Rc<Cell<Option<u64>>>,
    selection_anchor_id_for_key: Rc<Cell<Option<u64>>>,
    block_scroll_for_key: glib::WeakRef<ScrolledWindow>,
    bookmarks_for_key: Rc<RefCell<std::collections::HashSet<u64>>>,
    visible_indices_for_key: Rc<RefCell<std::collections::HashSet<usize>>>,
    bstate_for_key: Rc<Cell<BlockState>>,
}

impl KeyCtx {
    fn connect(self, key_ctrl: &gtk4::EventControllerKey) {
        let KeyCtx {
            pty_for_key,
            active_vte_for_key,
            pty_synced_for_key,
            bracketed_paste_for_key,
            typed_cmd_for_key,
            finished_blocks_for_key,
            block_data_for_key,
            block_list_for_key,
            selected_block_ids_for_key,
            selected_block_id_for_key,
            selection_anchor_id_for_key,
            block_scroll_for_key,
            bookmarks_for_key,
            visible_indices_for_key,
            bstate_for_key,
        } = self;
        key_ctrl.connect_key_pressed(move |_controller, keyval, _keycode, modifiers| {
            use gtk4::gdk::Key;
            let Some(active_vte_for_key) = active_vte_for_key.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let Some(block_list_for_key) = block_list_for_key.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let Some(block_scroll_for_key) = block_scroll_for_key.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let Some(finished_blocks_for_key) = finished_blocks_for_key.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let ctrl = modifiers.contains(gtk4::gdk::ModifierType::CONTROL_MASK);
            let shift = modifiers.contains(gtk4::gdk::ModifierType::SHIFT_MASK);
            let alt = modifiers.contains(gtk4::gdk::ModifierType::ALT_MASK);

            // History navigation stays local while the shell is idle. Once the
            // readline buffer has been edited, Home/End belong to the shell so
            // users can move to the beginning/end of the visible command.
            let state = bstate_for_key.get();
            let history_navigation = !matches!(
                state,
                BlockState::CollectingOutput | BlockState::AltScreen | BlockState::RawFallback
            );
            let editor_dirty = pty_synced_for_key.get() || !typed_cmd_for_key.borrow().is_empty();
            if !ctrl
                && !shift
                && !alt
                && history_edge_navigation_available(state, editor_dirty)
                && matches!(keyval, Key::Home | Key::End)
            {
                scroll_history_to_edge(&block_scroll_for_key, keyval == Key::End);
                return glib::Propagation::Stop;
            }
            if !ctrl
                && !shift
                && !alt
                && history_navigation
                && matches!(keyval, Key::Page_Up | Key::Page_Down)
            {
                let adj = block_scroll_for_key.vadjustment();
                let step = (adj.page_size() * 0.9).max(1.0);
                let delta = if keyval == Key::Page_Up { -step } else { step };
                let max_val = (adj.upper() - adj.page_size()).max(adj.lower());
                adj.set_value((adj.value() + delta).clamp(adj.lower(), max_val));
                return glib::Propagation::Stop;
            }

            // Shift+Up/Down expands or contracts the active range around a fixed
            // anchor. Without an active block the keys remain available to VTE.
            if !ctrl
                && shift
                && !alt
                && selected_block_id_for_key.get().is_some()
                && matches!(keyval, Key::Up | Key::Down)
            {
                let finished = finished_blocks_for_key.borrow();
                let direction = if keyval == Key::Up { -1 } else { 1 };
                if extend_finished_block_selection(
                    &finished,
                    &selected_block_ids_for_key,
                    &selected_block_id_for_key,
                    &selection_anchor_id_for_key,
                    &block_scroll_for_key,
                    direction,
                ) {
                    clear_vte_text_selections(&finished, &active_vte_for_key);
                    return glib::Propagation::Stop;
                }
            }

            // Once selection mode is active, plain Up/Down walks blocks. Without
            // a selection these still edit readline history in the live VTE.
            if !ctrl
                && !shift
                && !alt
                && selected_block_id_for_key.get().is_some()
                && matches!(keyval, Key::Up | Key::Down)
            {
                let finished = finished_blocks_for_key.borrow();
                let direction = if keyval == Key::Up { -1 } else { 1 };
                if move_finished_block_selection(
                    &finished,
                    &selected_block_ids_for_key,
                    &selected_block_id_for_key,
                    &selection_anchor_id_for_key,
                    &block_scroll_for_key,
                    direction,
                ) {
                    clear_vte_text_selections(&finished, &active_vte_for_key);
                    return glib::Propagation::Stop;
                }
            }

            // Ctrl+Shift+Up/Down aligns the selected card's top/bottom edge.
            if ctrl && shift && !alt && matches!(keyval, Key::Up | Key::Down) {
                let finished = finished_blocks_for_key.borrow();
                if scroll_selected_finished_block_edge(
                    &finished,
                    &selected_block_id_for_key,
                    &block_scroll_for_key,
                    keyval == Key::Down,
                ) {
                    return glib::Propagation::Stop;
                }
            }

            // Preserve the existing bracket aliases for entering and moving
            // block-selection mode without using the pointer.
            if ctrl && shift && !alt && matches!(keyval, Key::bracketleft | Key::bracketright) {
                let finished = finished_blocks_for_key.borrow();
                let direction = if keyval == Key::bracketleft { -1 } else { 1 };
                if move_finished_block_selection(
                    &finished,
                    &selected_block_ids_for_key,
                    &selected_block_id_for_key,
                    &selection_anchor_id_for_key,
                    &block_scroll_for_key,
                    direction,
                ) {
                    clear_vte_text_selections(&finished, &active_vte_for_key);
                    return glib::Propagation::Stop;
                }
            }

            // Enter recalls every selected command in terminal order as one
            // editable multiline buffer. It never steals Enter from a running process.
            if matches!(keyval, Key::Return | Key::KP_Enter) {
                if selected_block_id_for_key.get().is_some() {
                    let finished = finished_blocks_for_key.borrow();
                    // Programmatic paste/Agent input does not pass through this
                    // controller. If it has already dirtied the editor, Enter must
                    // submit the visible line rather than replacing it with a
                    // previously selected command.
                    if pty_synced_for_key.get() || !typed_cmd_for_key.borrow().is_empty() {
                        clear_finished_block_selection(
                            &finished,
                            &selected_block_ids_for_key,
                            &selected_block_id_for_key,
                            &selection_anchor_id_for_key,
                        );
                        return glib::Propagation::Proceed;
                    }
                    let recalled = {
                        let selected = selected_block_ids_for_key.borrow();
                        recall_selected_commands_at_prompt(
                            &pty_for_key,
                            &pty_synced_for_key,
                            &typed_cmd_for_key,
                            bstate_for_key.get(),
                            &finished,
                            &selected,
                            bracketed_paste_for_key.get(),
                        )
                    };
                    if recalled {
                        clear_finished_block_selection(
                            &finished,
                            &selected_block_ids_for_key,
                            &selected_block_id_for_key,
                            &selection_anchor_id_for_key,
                        );
                        return glib::Propagation::Stop;
                    }
                }
                return glib::Propagation::Proceed;
            }

            // Delete removes the selected block from both the document and saved
            // history. This is intentionally unmodified: selection is a visible,
            // explicit mode, while Backspace remains available to the shell.
            if !ctrl && !shift && !alt && keyval == Key::Delete {
                if let Some(sel_id) = selected_block_id_for_key.get() {
                    let next_id = remove_finished_block(
                        sel_id,
                        &finished_blocks_for_key,
                        &block_data_for_key,
                        &block_list_for_key,
                        BlockSelectionRefs {
                            ids: &selected_block_ids_for_key,
                            active: &selected_block_id_for_key,
                            anchor: &selection_anchor_id_for_key,
                        },
                        &bookmarks_for_key,
                        &visible_indices_for_key,
                    );
                    let finished = finished_blocks_for_key.borrow();
                    if selected_block_ids_for_key.borrow().is_empty() {
                        replace_finished_block_selection(
                            &finished,
                            &selected_block_ids_for_key,
                            &selected_block_id_for_key,
                            &selection_anchor_id_for_key,
                            next_id,
                        );
                    }
                    if let Some(next_id) = selected_block_id_for_key.get().or(next_id) {
                        if let Some(block) = finished.iter().find(|block| block.id == next_id) {
                            scroll_finished_block_into_view(&block_scroll_for_key, block);
                        }
                    }
                    return glib::Propagation::Stop;
                }
            }

            // Escape clears the block selection (when one is active).
            if keyval == Key::Escape {
                if selected_block_id_for_key.get().is_some() {
                    let finished = finished_blocks_for_key.borrow();
                    clear_finished_block_selection(
                        &finished,
                        &selected_block_ids_for_key,
                        &selected_block_id_for_key,
                        &selection_anchor_id_for_key,
                    );
                    return glib::Propagation::Stop;
                }
                return glib::Propagation::Proceed;
            }

            // Linux Warp toggles the selected/latest block's output filter with Alt+Shift+F.
            if alt
                && shift
                && !ctrl
                && matches!(keyval, Key::f | Key::F)
                && bstate_for_key.get() != BlockState::AltScreen
            {
                let finished = finished_blocks_for_key.borrow();
                let target = selected_block_id_for_key
                    .get()
                    .and_then(|id| finished.iter().find(|block| block.id == id))
                    .or_else(|| finished.last());
                if let Some(block) = target {
                    (block.toggle_filter)();
                    return glib::Propagation::Stop;
                }
            }

            // Ctrl+Shift+B: toggle a bookmark on the selected block (Warp's
            // Linux binding). Shows the gutter star + accent stripe.
            // Only consume the key when bookmark logic actually fires — in
            // alt-screen (vim/less) or with no selection, let VTE deliver
            // Ctrl+Shift+B to the running app.
            if ctrl
                && shift
                && !alt
                && matches!(keyval, Key::b | Key::B)
                && bstate_for_key.get() != BlockState::AltScreen
            {
                if let Some(sel_id) = selected_block_id_for_key.get() {
                    let finished = finished_blocks_for_key.borrow();
                    if let Some(block) = finished.iter().find(|b| b.id == sel_id) {
                        let mut marks = bookmarks_for_key.borrow_mut();
                        let now_marked = if marks.remove(&sel_id) {
                            false
                        } else {
                            marks.insert(sel_id);
                            true
                        };
                        block.bookmark_star.set_visible(now_marked);
                        if now_marked {
                            block.widget().add_css_class("block-bookmarked");
                        } else {
                            block.widget().remove_css_class("block-bookmarked");
                        }
                        return glib::Propagation::Stop;
                    }
                }
            }

            // Ctrl+,/Ctrl+. : jump to the previous/next bookmarked block (Warp's
            // SelectBookmarkUp/Down). VTE swallows Alt+arrow and plain Ctrl+arrow
            // before the capture handler sees them, so comma/period are used here.
            if ctrl && !alt && !shift && matches!(keyval, Key::comma | Key::period) {
                if bstate_for_key.get() == BlockState::AltScreen {
                    return glib::Propagation::Proceed;
                }
                let finished = finished_blocks_for_key.borrow();
                let marks = bookmarks_for_key.borrow();
                if marks.is_empty() {
                    return glib::Propagation::Proceed;
                }
                let marked_idx: Vec<usize> = finished
                    .iter()
                    .enumerate()
                    .filter(|(_, b)| marks.contains(&b.id))
                    .map(|(i, _)| i)
                    .collect();
                if marked_idx.is_empty() {
                    return glib::Propagation::Proceed;
                }
                let cur = selected_block_id_for_key
                    .get()
                    .and_then(|id| finished.iter().position(|b| b.id == id));
                let target = if keyval == Key::comma {
                    marked_idx
                        .iter()
                        .rev()
                        .find(|&&i| cur.map(|c| i < c).unwrap_or(true))
                        .copied()
                        .or_else(|| marked_idx.last().copied())
                } else {
                    marked_idx
                        .iter()
                        .find(|&&i| cur.map(|c| i > c).unwrap_or(true))
                        .copied()
                        .or_else(|| marked_idx.first().copied())
                };
                if let Some(idx) = target {
                    clear_vte_text_selections(&finished, &active_vte_for_key);
                    let new_id = finished.get(idx).map(|b| b.id);
                    replace_finished_block_selection(
                        &finished,
                        &selected_block_ids_for_key,
                        &selected_block_id_for_key,
                        &selection_anchor_id_for_key,
                        new_id,
                    );
                    if let Some(block) = finished.get(idx) {
                        scroll_finished_block_into_view(&block_scroll_for_key, block);
                    }
                }
                return glib::Propagation::Stop;
            }

            // Ctrl+Shift+C / Ctrl+Shift+V are handled at the window-level
            // capture handler in main.rs (via TermView::copy_to_clipboard /
            // paste_from_clipboard) so they work regardless of which child
            // widget currently has focus — in particular after the user
            // mouse-selects text inside a finished block's TextView, focus
            // sits there and this per-VTE controller never fires.

            // Plain Ctrl+P belongs to readline and terminal applications. The
            // app-level Ctrl+Shift+H action owns command-history recall.

            // Everything else: let the VTE translate it (printable keys, editing,
            // control sequences, IME) and emit `commit`.
            glib::Propagation::Proceed
        });
    }
}

#[allow(dead_code)]
impl TermView {
    /// Replace the runtime configuration shared by parser/render callbacks.
    /// Existing widgets receive their visual updates through UiState; this
    /// updates behavioral options such as notifications, filtering, mouse
    /// reporting, history limits, and clipboard policy for subsequent events.
    pub(crate) fn reload_config(&self, config: &Config) {
        *self.config.borrow_mut() = config.clone();
    }

    pub fn new(
        config: &Config,
        shell_argv: &[String],
        cwd: Option<&str>,
        session_id: Option<&str>,
        initial_commands: &[String],
    ) -> io::Result<Self> {
        Self::new_with_spawner(
            config,
            shell_argv,
            cwd,
            session_id,
            initial_commands,
            OwnedPty::spawn,
        )
    }

    /// Constructor boundary with an injectable PTY spawner.
    ///
    /// Spawning happens before any GTK object is allocated. A missing shell or
    /// exhausted PTY/process resource therefore returns a diagnostic error with
    /// no half-built widget tree for callers to clean up. Keeping the boundary
    /// injectable also makes the failure contract testable without a display.
    fn new_with_spawner<F>(
        config: &Config,
        shell_argv: &[String],
        cwd: Option<&str>,
        session_id: Option<&str>,
        initial_commands: &[String],
        spawn: F,
    ) -> io::Result<Self>
    where
        F: FnOnce(&[&str], Option<&str>, &[(&str, &str)]) -> io::Result<OwnedPty>,
    {
        // Detect jsh shell for session_id passing.
        let is_jsh = shell_argv
            .first()
            .and_then(|s| std::path::Path::new(s).file_name())
            .and_then(|f| f.to_str())
            .map(|name| name == "jsh")
            .unwrap_or(false);

        let session_id = session_id.filter(|sid| crate::review_input::valid_jsh_id(sid));

        // Build argv with optional --session for jsh.
        let mut argv_vec: Vec<String> = shell_argv.to_vec();
        if let Some(sid) = session_id {
            if is_jsh {
                argv_vec.push("--session".to_string());
                argv_vec.push(sid.to_string());
            }
        }
        let argv: Vec<&str> = argv_vec.iter().map(String::as_str).collect();

        // Only this pane's own variables belong here. `TERM_PROGRAM` (which the
        // documented `[[ $TERM_PROGRAM == jterm4 ]] && source ...` rc gate reads)
        // and the `LESS=R` pager default come from `child_env` inside the PTY
        // spawner, so the fork site and the Flatpak host bridge cannot drift.
        let mut env_extra: Vec<(&str, &str)> = Vec::new();
        let session_id_owned = session_id.map(str::to_owned);
        if let Some(ref sid) = session_id_owned {
            if is_jsh {
                env_extra.push(("JSH_SESSION_ID", sid.as_str()));
            }
        }

        // Every cwd-shaped failure is absorbed inside `OwnedPty::spawn`: a
        // restored session pointing at a deleted worktree or unmounted drive
        // starts in the application directory. Missing executables and resource
        // exhaustion retain their original io::Error for the transactional UI
        // caller to log and present instead of panicking the application.
        let pty = Rc::new(spawn(&argv, cwd, &env_extra)?);
        // ── Build widget tree ──────────────────────────────────────────────
        let root = gtk4::Box::new(Orientation::Vertical, 0);
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_focusable(true);
        root.add_css_class("term-view-root");
        // Stays hidden until this pane's tab is split.
        let pane_header = crate::ui::PaneHeader::new();
        root.append(pane_header.widget());

        // Block list inside a scrolled window
        let block_list = gtk4::Box::new(Orientation::Vertical, 0);
        block_list.set_vexpand(true);
        block_list.add_css_class("block-list");

        let block_scroll = ScrolledWindow::new();
        block_scroll.set_hexpand(true);
        block_scroll.set_vexpand(true);
        block_scroll.set_policy(gtk4::PolicyType::Automatic, gtk4::PolicyType::Automatic);
        block_scroll.set_child(Some(&block_list));
        block_scroll.add_css_class("block-scroll");
        // A focusable ScrolledWindow steals keyboard focus from the live VTE
        // child (cursor goes hollow, keystrokes never reach the terminal). Make
        // it not a focus target so focus delegates to the VTE. NOTE: use
        // `focusable(false)`, NOT `can_focus(false)` — in GTK4 `can-focus=false`
        // blocks the whole subtree (including the VTE) from ever taking focus.
        block_scroll.set_focusable(false);

        // Active block: a single persistent live VTE pinned at the bottom of the
        // block list. Prompt + typing + output all render here natively (jterm1
        // model); finished commands snapshot into styled blocks above it.
        let active = Rc::new(RefCell::new(ActiveBlock::new(config)));
        let active_vte = active.borrow().active_vte.clone();

        block_list.append(active.borrow().widget());

        // The live VTE is visually compact at a prompt and expands to the full
        // viewport for running commands and terminal apps. PTY geometry remains
        // viewport-sized in both cases.

        // ── Jump-to-bottom floating action button ─────────────────────────
        // Shown when the user scrolls up into history; an optional unread badge
        // counts finished blocks that completed while scrolled away. Clicking it
        // returns the view to the live prompt. Overlaid on the scroll area so it
        // floats over the block list without taking layout space.
        let jump_fab = gtk4::Button::new();
        jump_fab.add_css_class("jump-bottom-fab");
        jump_fab.add_css_class("flat");
        jump_fab.set_label("\u{f078}"); // nf-fa-chevron_down
        jump_fab.set_tooltip_text(Some("Jump to latest"));
        jump_fab.set_halign(gtk4::Align::End);
        jump_fab.set_valign(gtk4::Align::End);
        jump_fab.set_margin_end(18);
        jump_fab.set_margin_bottom(18);
        jump_fab.set_visible(false);
        jump_fab.set_can_focus(false);

        // ── Sticky running-command header ─────────────────────────────────
        // When a command is running and the user has scrolled up into history,
        // a thin bar pins to the top of the scroll area showing the live command
        // and its elapsed time, so they don't lose track of what's executing.
        let sticky_label = gtk4::Label::new(None);
        sticky_label.set_halign(gtk4::Align::Start);
        sticky_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        sticky_label.set_hexpand(true);
        sticky_label.add_css_class("sticky-running-label");
        let sticky_jump_bottom_btn = gtk4::Button::with_label("\u{f103}");
        sticky_jump_bottom_btn.set_tooltip_text(Some("Jump to bottom of this block"));
        sticky_jump_bottom_btn.add_css_class("sticky-header-control");
        sticky_jump_bottom_btn.add_css_class("flat");
        sticky_jump_bottom_btn.set_focusable(false);
        sticky_jump_bottom_btn.set_visible(false);
        let sticky_minimize_btn = gtk4::Button::with_label("\u{f077}");
        sticky_minimize_btn.set_tooltip_text(Some("Minimize sticky command header"));
        sticky_minimize_btn.add_css_class("sticky-header-control");
        sticky_minimize_btn.add_css_class("flat");
        sticky_minimize_btn.set_focusable(false);
        // Interrupt without hunting for terminal focus: while reading history
        // above a running command, one click sends Ctrl+C. Wired to the PTY
        // further down, once it exists.
        let sticky_stop_btn = gtk4::Button::with_label("\u{f04d}");
        sticky_stop_btn.set_tooltip_text(Some("Interrupt the running command (Ctrl+C)"));
        sticky_stop_btn.add_css_class("sticky-header-control");
        sticky_stop_btn.add_css_class("flat");
        sticky_stop_btn.set_focusable(false);
        sticky_stop_btn.set_visible(false);
        let sticky_bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        sticky_bar.add_css_class("sticky-running-header");
        sticky_bar.append(&sticky_label);
        sticky_bar.append(&sticky_stop_btn);
        sticky_bar.append(&sticky_jump_bottom_btn);
        sticky_bar.append(&sticky_minimize_btn);
        sticky_bar.set_halign(gtk4::Align::Fill);
        sticky_bar.set_valign(gtk4::Align::Start);
        sticky_bar.set_visible(false);
        sticky_bar.set_can_focus(false);
        let sticky_target_id: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
        let sticky_minimized: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        {
            let minimized = sticky_minimized.clone();
            let label = sticky_label.downgrade();
            let jump = sticky_jump_bottom_btn.downgrade();
            let stop = sticky_stop_btn.downgrade();
            let bar = sticky_bar.downgrade();
            sticky_minimize_btn.connect_clicked(move |button| {
                let (Some(label), Some(jump), Some(bar)) =
                    (label.upgrade(), jump.upgrade(), bar.upgrade())
                else {
                    return;
                };
                let now = !minimized.get();
                minimized.set(now);
                label.set_visible(!now);
                jump.set_visible(false);
                // The 250ms sticky refresh restores it when expanding.
                if let Some(stop) = stop.upgrade() {
                    stop.set_visible(false);
                }
                if now {
                    bar.add_css_class("sticky-minimized");
                    button.set_label("\u{f078}");
                    button.set_tooltip_text(Some("Expand sticky command header"));
                } else {
                    bar.remove_css_class("sticky-minimized");
                    button.set_label("\u{f077}");
                    button.set_tooltip_text(Some("Minimize sticky command header"));
                }
            });
        }

        let scroll_overlay = gtk4::Overlay::new();
        scroll_overlay.set_child(Some(&block_scroll));
        scroll_overlay.add_overlay(&sticky_bar);
        scroll_overlay.add_overlay(&jump_fab);
        root.append(&scroll_overlay);

        // ── Repo-status strip ────────────────────────────────────────────
        // A thin always-visible label at the bottom showing the current
        // pane's git branch + dirty marker + ahead/behind. Refreshed on
        // cwd change and on every finished block (the user may have just
        // run `git commit` or `git pull`). Hidden when cwd isn't a repo.
        let repo_strip = gtk4::Label::new(None);
        repo_strip.set_halign(gtk4::Align::Start);
        repo_strip.set_xalign(0.0);
        repo_strip.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        repo_strip.add_css_class("repo-strip");
        repo_strip.set_visible(false);
        if config.show_repo_strip {
            root.append(&repo_strip);
        }

        let unread_count: Rc<Cell<u32>> = Rc::new(Cell::new(0));

        // ── PTY ───────────────────────────────────────────────────────────
        // Share the child lifecycle with the live VTE so widget-tree teardown
        // (kill_all_terminal_children, tab close) terminates exactly the same
        // child this pane owns — through the same handle, so a close and this
        // pane's own drop cannot produce two escalations. Unlike a
        // conventional pane, this child was forked by jterm4 itself and is
        // reaped here, which the shared lifecycle records.
        crate::terminal::set_terminal_child_lifecycle(&active_vte, pty.lifecycle());

        // ── Register CSS ──────────────────────────────────────────────────
        install_block_css(config);

        // ── Shared state ──────────────────────────────────────────────────
        let bstate = Rc::new(Cell::new(BlockState::Idle));

        // Keystroke-shadow command line. The authoritative command text is read
        // off the VTE at CommandStart; this remains a best-effort fallback when
        // a shell-integration anchor cannot be captured.
        let typed_cmd: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let external_submission: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let background_output: Rc<RefCell<BoundedByteRing>> =
            Rc::new(RefCell::new(BoundedByteRing::new(MAX_RAW_OUTPUT_BYTES)));
        let idle_input_dirty: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // Command text snapshot taken at CommandStart from the VTE itself,
        // between `prompt_end_pos` and the current cursor. This is what
        // finalize uses to record the run.
        let vte_typed_cmd: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        // VTE cursor position (col, row) right after the prompt finished
        // drawing — anchor for the text-range read at CommandStart.
        let prompt_end_pos: Rc<Cell<(i64, i64)>> = Rc::new(Cell::new((0, 0)));

        // Scroll-lock flags shared across the contents_changed pin, value_changed
        // detector, FAB, and ScrollDebouncer. `user_scrolled_up` suppresses the
        // follow-bottom pin while the user is reading history; `programmatic_scroll`
        // marks our own adjustment writes so the value_changed detector doesn't
        // mistake them for a user drag.
        let user_scrolled_up: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let programmatic_scroll: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let scroll_debouncer = ScrollDebouncer::with_scroll_lock(
            user_scrolled_up.clone(),
            programmatic_scroll.clone(),
        );

        let block_data_rc: Rc<RefCell<VecDeque<BlockData>>> =
            Rc::new(RefCell::new(VecDeque::new()));
        let finished_blocks_rc: Rc<RefCell<Vec<FinishedBlock>>> = Rc::new(RefCell::new(Vec::new()));

        // ── Hybrid live-surface layout ─────────────────────────────────────
        // Idle prompts use a compact visual cell so completed output exists only
        // once, in blocks above. Running commands and terminal apps receive the
        // full live VTE. PTY rows are NOT taken from this visual height; see
        // `pty_grid_size`, which always reports the full viewport to the child.
        let layout_active_surface: Rc<dyn Fn()> = {
            let holder = active.borrow().widget().downgrade();
            let vte = active_vte.downgrade();
            let scroll = block_scroll.downgrade();
            let bstate = bstate.clone();
            let typed_cmd = typed_cmd.clone();
            let finished_for_layout = finished_blocks_rc.clone();
            let block_data_for_layout = block_data_rc.clone();
            let last_size_target: Rc<Cell<(i64, i64)>> = Rc::new(Cell::new((0, 0)));
            // Change detector for the finished-block re-fit. Their cap follows
            // the scroll viewport's pixel height and the cell height (font
            // zoom), and nothing else — so this runs once per real geometry
            // change rather than on every contents-changed signal.
            let last_output_layout: Rc<Cell<(i32, i32)>> = Rc::new(Cell::new((-1, -1)));
            Rc::new(move || {
                let Some(holder) = holder.upgrade() else {
                    return;
                };
                let Some(vte) = vte.upgrade() else {
                    return;
                };
                let Some(scroll) = scroll.upgrade() else {
                    return;
                };
                let cell_h = (vte.char_height() as i32).max(1);
                let Some(viewport_rows) = viewport_rows_for(&vte, &scroll) else {
                    return;
                };
                let cols = vte.column_count().max(1);
                holder.set_visible(true);
                let compact_rows = {
                    let input_lines =
                        1 + typed_cmd.borrow().bytes().filter(|&b| b == b'\n').count() as i64;
                    let floor = (MIN_INPUT_ROWS as i64).min(viewport_rows);
                    input_lines.clamp(floor, viewport_rows.max(floor))
                };
                let target_rows = match bstate.get() {
                    BlockState::Idle
                    | BlockState::CollectingPrompt
                    | BlockState::AwaitingCommand => compact_rows,
                    BlockState::CollectingOutput
                    | BlockState::PostCommand
                    | BlockState::AltScreen
                    | BlockState::RawFallback => viewport_rows,
                };
                let target = (cols, target_rows);
                if last_size_target.get() != target {
                    vte.set_size(cols, target_rows);
                    last_size_target.set(target);
                }
                holder.set_height_request((target_rows as i32) * cell_h);

                // Re-fit already-visible finished blocks to the resized pane.
                // Blocks that scroll off and back are handled by their own map
                // pass; this reaches the ones that never unmapped.
                let page_height = scroll.vadjustment().page_size() as i32;
                let layout_key = (page_height, cell_h);
                if last_output_layout.replace(layout_key) == layout_key {
                    return;
                }
                // Collect first, write after. Re-fitting touches GTK widgets,
                // and this runs from a size-allocate signal; holding the
                // metadata borrow across that would turn any re-entrant layout
                // pass into a RefCell panic.
                let resized: Vec<(u64, i32)> = {
                    let finished = finished_for_layout.borrow();
                    finished
                        .iter()
                        .filter_map(|block| block.refit_output_to_viewport().map(|h| (block.id, h)))
                        .collect()
                };
                if resized.is_empty() {
                    return;
                }
                let mut block_data = block_data_for_layout.borrow_mut();
                for (id, height) in resized {
                    if let Some(data) = block_data.iter_mut().find(|data| data.id == id) {
                        data.estimated_height = height;
                    }
                }
            })
        };
        // Coalesces follow-bottom pins so a burst of contents-changed signals
        // schedules at most one deferred scroll.
        let pin_pending: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        {
            // Reassert the viewport grid from the data path and follow the
            // bottom from here too — NOT from the vadjustment `changed` signal.
            //
            // Why a deferred idle and not `changed`: pinning inside `changed`
            // reacts to virtualization's own `upper` changes (off-screen blocks
            // collapse to 0 height when hidden), so pin → hide top block → upper
            // shrinks → `changed` → pin → block reappears → upper grows → `changed`
            // → … an infinite two-state oscillation. A low-priority idle runs once
            // per content burst, AFTER layout settles (so `upper` is final), and is
            // never re-triggered by the visibility side-effects of its own scroll.
            let f = layout_active_surface.clone();
            let scroll = block_scroll.downgrade();
            let user_scrolled = user_scrolled_up.clone();
            let programmatic = programmatic_scroll.clone();
            let pin_pending = pin_pending.clone();
            active_vte.connect_contents_changed(move |_| {
                f();
                if user_scrolled.get() || pin_pending.get() {
                    return;
                }
                pin_pending.set(true);
                let scroll = scroll.clone();
                let user_scrolled = user_scrolled.clone();
                let programmatic = programmatic.clone();
                let pin_pending = pin_pending.clone();
                glib::idle_add_local_once(move || {
                    pin_pending.set(false);
                    if user_scrolled.get() {
                        return;
                    }
                    let Some(scroll) = scroll.upgrade() else {
                        return;
                    };
                    let adj = scroll.vadjustment();
                    let target = (adj.upper() - adj.page_size()).max(adj.lower());
                    if (adj.value() - target).abs() > 1.0 {
                        programmatic.set(true);
                        adj.set_value(target);
                        programmatic.set(false);
                    }
                });
            });
        }

        // State to restore when an alt-screen app exits (jterm1 model).
        let prev_state: Rc<Cell<BlockState>> = Rc::new(Cell::new(BlockState::Idle));
        let osc133_depth: Rc<Cell<u32>> = Rc::new(Cell::new(0));
        let prompt_buf: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        // Rendered prompt captured at PromptEnd (prompt_buf is cleared once the
        // prompt ends, so the finalize path reads this instead).
        let prompt_display: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        // True while an alt-screen app owns the viewport (finished blocks hidden).
        let fullscreen: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let cwd_callbacks: StrCallbacks = Rc::new(RefCell::new(vec![]));
        let remote_session_callbacks: StrCallbacks = Rc::new(RefCell::new(vec![]));
        let exited_callbacks: IntCallbacks = Rc::new(RefCell::new(vec![]));
        let bell_callbacks: VoidCallbacks = Rc::new(RefCell::new(vec![]));
        // Bell signal is delivered natively by VTE — no need to scan the byte
        // stream for BEL ourselves (and disambiguate it from OSC string
        // terminators). VTE already does that disambiguation inside its parser.
        {
            let bell_cbs = bell_callbacks.clone();
            active_vte.connect_bell(move |_| {
                for cb in bell_cbs.borrow().iter() {
                    cb();
                }
            });
        }
        let title_callbacks: StrCallbacks = Rc::new(RefCell::new(vec![]));
        let activity_callbacks: VoidCallbacks = Rc::new(RefCell::new(vec![]));
        let block_finished_callbacks: BlockFinishedCallbacks = Rc::new(RefCell::new(vec![]));
        let mouse_reporting_mode: Rc<Cell<MouseReportingMode>> =
            Rc::new(Cell::new(MouseReportingMode::None));
        // Unlike a regular VTE terminal, block mode owns the shell PTY. Keep
        // DECSET 2004 state here so clipboard pastes can be forwarded as one
        // ordered byte stream instead of relying on VTE's unrelated PTY.
        let bracketed_paste: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        // `None` until a CommandEnd reports one. A shell that sends the bare
        // FinalTerm `D` mark with no status leaves it None, which must not be
        // rendered as a success.
        let pending_exit_code: Rc<Cell<Option<i32>>> = Rc::new(Cell::new(None));
        let pending_command_meta: Rc<RefCell<PendingCommandMeta>> =
            Rc::new(RefCell::new(PendingCommandMeta::default()));

        let widget_pool: Rc<RefCell<WidgetPool>> = Rc::new(RefCell::new(WidgetPool::new()));
        let pty_synced: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let selected_block_ids: SelectedBlockIds =
            Rc::new(RefCell::new(std::collections::HashSet::new()));
        let selected_block_id: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
        let selection_anchor_id: Rc<Cell<Option<u64>>> = Rc::new(Cell::new(None));
        // Bookmarked block ids (in-memory for the session). Toggled with Ctrl+Shift+B;
        // navigated with Ctrl+,/Ctrl+.. Not persisted (avoids an rkyv schema bump).
        let block_bookmarks: Rc<RefCell<std::collections::HashSet<u64>>> =
            Rc::new(RefCell::new(std::collections::HashSet::new()));
        {
            let target = sticky_target_id.clone();
            let finished = finished_blocks_rc.clone();
            let scroll = block_scroll.clone();
            let click = gtk4::GestureClick::new();
            click.set_button(1);
            click.connect_released(move |_, n_press, _, _| {
                if n_press != 1 {
                    return;
                }
                let Some(id) = target.get() else {
                    return;
                };
                let finished = finished.borrow();
                let Some(block) = finished.iter().find(|block| block.id == id) else {
                    return;
                };
                block.scroll_to_edge(&scroll, false);
            });
            sticky_label.add_controller(click);
        }
        {
            let target = sticky_target_id.clone();
            let finished = finished_blocks_rc.clone();
            let scroll = block_scroll.clone();
            sticky_jump_bottom_btn.connect_clicked(move |_| {
                let Some(id) = target.get() else {
                    return;
                };
                let finished = finished.borrow();
                let Some(block) = finished.iter().find(|block| block.id == id) else {
                    return;
                };
                block.scroll_to_edge(&scroll, true);
            });
        }
        // Parks PTY chunks while the user drag-selects on the live VTE, so a
        // running command's repaints can't clear the selection out from under
        // the pointer. Shared by the PTY reader (parking/replay) and the
        // cross-selection gestures (drag lifecycle).
        let selection_feed_hold = SelectionFeedHold::new();
        // Frozen output with no explanation reads as a hang. A small badge
        // appears only once bytes are actually parked (a plain click on the
        // live surface must not flash it) and disappears on flush.
        {
            let hold_badge = gtk4::Label::new(Some("\u{f04c}  Output paused — selection"));
            hold_badge.add_css_class("feed-hold-badge");
            hold_badge.set_tooltip_text(Some(
                "Streaming output is held so your selection survives. Copy it, \
                 click elsewhere, or wait a few seconds to resume.",
            ));
            hold_badge.set_halign(gtk4::Align::Start);
            hold_badge.set_valign(gtk4::Align::End);
            hold_badge.set_margin_start(14);
            hold_badge.set_margin_bottom(14);
            hold_badge.set_visible(false);
            hold_badge.set_can_focus(false);
            scroll_overlay.add_overlay(&hold_badge);
            let badge = hold_badge.downgrade();
            selection_feed_hold.set_state_listener(move |parked| {
                if let Some(badge) = badge.upgrade() {
                    badge.set_visible(parked);
                }
            });
        }
        // Sticky running-command header state: true while a command is executing,
        // plus the command text captured at CommandStart.
        let cmd_running: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let running_cmd: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let block_start_time: Rc<Cell<Option<SystemTime>>> = Rc::new(Cell::new(None));
        let visible_indices: Rc<RefCell<std::collections::HashSet<usize>>> =
            Rc::new(RefCell::new(std::collections::HashSet::new()));
        // Set once any OSC-133 (FTCS) event is seen, so the view knows shell
        // integration is live.
        let ftcs_seen: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let current_cwd: Rc<RefCell<String>> = Rc::new(RefCell::new(cwd.unwrap_or("").to_string()));

        // CWD updates come from VTE's native OSC 7 signal (the parser passes
        // OSC 7 through unchanged, see parser.rs). Title updates likewise come
        // from VTE's window-title-changed (OSC 0/2).
        {
            let cwd_cbs = cwd_callbacks.clone();
            let current_cwd_for_signal = current_cwd.clone();
            let repo_strip_for_cwd = repo_strip.clone();
            let fullscreen_for_cwd = fullscreen.clone();
            active_vte.connect_current_directory_uri_notify(move |terminal| {
                if let Some(uri) = terminal.current_directory_uri() {
                    let file = gtk4::gio::File::for_uri(uri.as_str());
                    if let Some(path) = file
                        .path()
                        .map(|p| p.to_string_lossy().to_string())
                        .filter(|s| !s.is_empty())
                    {
                        *current_cwd_for_signal.borrow_mut() = path.clone();
                        if fullscreen_for_cwd.get() {
                            repo_strip_for_cwd.set_visible(false);
                        } else {
                            refresh_repo_strip(&repo_strip_for_cwd, &path);
                        }
                        let display_path =
                            crate::review_input::safe_inline_display(&path, 4 * 1024);
                        for cb in cwd_cbs.borrow().iter() {
                            cb(&display_path);
                        }
                    }
                }
            });
        }

        // Initial probe so the strip is populated for the starting cwd
        // before the user has cd'd anywhere (the OSC 7 above only fires
        // on a change).
        {
            let initial_cwd = current_cwd.borrow().clone();
            refresh_repo_strip(&repo_strip, &initial_cwd);
        }
        {
            let title_cbs = title_callbacks.clone();
            active_vte.connect_window_title_changed(move |terminal| {
                if let Some(title) = terminal.window_title() {
                    let title_str = crate::review_input::safe_inline_display(&title, 512);
                    if !title_str.is_empty() {
                        for cb in title_cbs.borrow().iter() {
                            cb(&title_str);
                        }
                    }
                }
            });
        }

        // ── Wire PTY → parser → block events ─────────────────────────────
        {
            let active_rc = active.clone();
            let active_vte_rc = active_vte.clone();
            let bstate_rc = bstate.clone();
            let prev_state_rc = prev_state.clone();
            let osc133_depth_rc = osc133_depth.clone();
            let prompt_buf_rc = prompt_buf.clone();
            let typed_cmd_rc = typed_cmd.clone();
            let vte_typed_cmd_rc = vte_typed_cmd.clone();
            let prompt_end_pos_rc = prompt_end_pos.clone();
            let prompt_display_rc = prompt_display.clone();
            let block_list_rc = block_list.clone();
            let block_scroll_rc = block_scroll.clone();
            let exited_cbs = exited_callbacks.clone();
            let activity_cbs = activity_callbacks.clone();
            let mouse_reporting_rc = mouse_reporting_mode.clone();
            let bracketed_paste_rc = bracketed_paste.clone();
            let config_for_cb = Rc::new(RefCell::new(config.clone()));
            let parser = Rc::new(RefCell::new(Parser::with_config(ParserConfig {
                mouse_reporting: config.mouse_reporting_enabled,
                focus_reporting: config.focus_reporting_enabled,
            })));
            let block_data_for_cb = block_data_rc.clone();
            let finished_blocks_for_cb = finished_blocks_rc.clone();
            let widget_pool_for_cb = widget_pool.clone();
            let pty_synced_rc = pty_synced.clone();
            let visible_indices_rc = visible_indices.clone();
            let fullscreen_rc = fullscreen.clone();
            let ftcs_seen_rc = ftcs_seen.clone();

            // Command queue for replaying initial_commands on PromptEnd events.
            // Commands are pre-parsed at the application boundary; splitting
            // here would reinterpret a restored command's own bytes.
            let init_cmds_queue: Rc<RefCell<std::collections::VecDeque<String>>> =
                Rc::new(RefCell::new(initial_commands.iter().cloned().collect()));
            let init_cmds_queue_for_cb = Rc::clone(&init_cmds_queue);
            let pty_for_init = Rc::clone(&pty);
            let block_start_time_for_cb = block_start_time.clone();
            let pending_exit_code_rc = pending_exit_code.clone();
            let pending_command_meta_rc = pending_command_meta.clone();
            let current_cwd_for_cb = current_cwd.clone();

            let event_buf: Rc<RefCell<Vec<ParserEvent>>> =
                Rc::new(RefCell::new(Vec::with_capacity(32)));
            ReaderCtx {
                active_rc,
                active_vte: active_vte_rc,
                bstate_rc,
                prev_state_rc,
                osc133_depth_rc,
                prompt_buf_rc,
                typed_cmd_rc,
                external_submission_rc: external_submission.clone(),
                background_output_rc: background_output.clone(),
                idle_input_dirty_rc: idle_input_dirty.clone(),
                vte_typed_cmd_rc,
                prompt_end_pos_rc,
                prompt_display_rc,
                block_list_rc,
                block_scroll_rc,
                remote_session_cbs: remote_session_callbacks.clone(),
                exited_cbs,
                activity_cbs,
                mouse_reporting_rc,
                bracketed_paste_rc,
                config_for_cb,
                parser,
                block_data_for_cb,
                finished_blocks_for_cb,
                scroll_debouncer: scroll_debouncer.clone(),
                widget_pool_for_cb,
                pty_synced_rc,
                visible_indices_rc,
                fullscreen_rc,
                ftcs_seen_rc,
                init_cmds_queue_for_cb,
                pty_for_init,
                block_start_time_for_cb,
                pending_exit_code_rc,
                pending_command_meta_rc,
                current_cwd_for_cb,
                event_buf,
                unread_count_rc: unread_count.clone(),
                jump_fab: jump_fab.clone(),
                sticky_bar: sticky_bar.clone(),
                selected_block_ids_rc: selected_block_ids.clone(),
                selected_block_id_rc: selected_block_id.clone(),
                selection_anchor_id_rc: selection_anchor_id.clone(),
                bookmarks_for_cb: block_bookmarks.clone(),
                cmd_running_rc: cmd_running.clone(),
                running_cmd_rc: running_cmd.clone(),
                layout_active_surface: layout_active_surface.clone(),
                repo_strip: repo_strip.clone(),
                block_finished_cbs: block_finished_callbacks.clone(),
                selection_feed_hold: selection_feed_hold.clone(),
            }
            .install(&pty);
        }

        // ── Scroll lock + jump-to-bottom FAB ──────────────────────────────
        // The block list virtualizes (off-screen finished blocks are hidden →
        // 0 height), so `adjustment.upper()` shrinks as you scroll and the usual
        // value-vs-max "at bottom" math can never be trusted. Instead detect the
        // live bottom geometrically off the never-virtualized live VTE holder.
        //
        // Compact and full-screen live layouts have different heights, so detect
        // the invariant that matters: whether the live holder still intersects
        // the viewport. Once its top moves below the viewport, the user is reading
        // history and follow mode must stop. Sample on idle after layout settles.
        {
            let user_scrolled = user_scrolled_up.clone();
            let fab = jump_fab.downgrade();
            let unread = unread_count.clone();
            let scroll = block_scroll.downgrade();
            let holder = active.borrow().widget().downgrade();
            let programmatic_scroll = programmatic_scroll.clone();
            let fullscreen = fullscreen.clone();
            let check_pending = Rc::new(Cell::new(false));
            let pending_programmatic_only = Rc::new(Cell::new(true));
            block_scroll
                .vadjustment()
                .connect_value_changed(move |_adj| {
                    // `set_value()` emits this synchronously, while the geometry
                    // check below deliberately runs on idle. Preserve the source
                    // now: otherwise the programmatic flag has been cleared by
                    // the time the idle runs and a follow-bottom pin is mistaken
                    // for the user scrolling into history.
                    let caused_by_programmatic_scroll = programmatic_scroll.get();
                    if check_pending.get() {
                        if !caused_by_programmatic_scroll {
                            pending_programmatic_only.set(false);
                        }
                        return;
                    }
                    check_pending.set(true);
                    pending_programmatic_only.set(caused_by_programmatic_scroll);
                    let user_scrolled = user_scrolled.clone();
                    let fab = fab.clone();
                    let unread = unread.clone();
                    let scroll = scroll.clone();
                    let holder = holder.clone();
                    let fab = fab.clone();
                    let fullscreen = fullscreen.clone();
                    let check_pending = check_pending.clone();
                    let pending_programmatic_only = pending_programmatic_only.clone();
                    glib::idle_add_local_once(move || {
                        check_pending.set(false);
                        if pending_programmatic_only.replace(true) {
                            return;
                        }
                        let Some(fab) = fab.upgrade() else {
                            return;
                        };
                        let Some(scroll) = scroll.upgrade() else {
                            return;
                        };
                        let Some(holder) = holder.upgrade() else {
                            return;
                        };
                        if fullscreen.get() {
                            user_scrolled.set(false);
                            unread.set(0);
                            fab.set_visible(false);
                            return;
                        }
                        let vp_h = scroll.height() as f64;
                        let at_bottom = holder
                            .compute_bounds(&scroll)
                            .map(|b| (b.y() as f64) < vp_h - 4.0)
                            .unwrap_or(true);
                        user_scrolled.set(!at_bottom);
                        if at_bottom {
                            unread.set(0);
                            fab.set_visible(false);
                        } else {
                            set_jump_fab_label(&fab, unread.get());
                            fab.set_visible(true);
                        }
                    });
                });
        }

        // ── Recompute the live grid on viewport resize ────────────────────
        // `changed` fires during the viewport's size-allocate, after layout. We
        // re-clamp the input height here ONLY when the viewport itself resized
        // (page_size moved) — content-driven sizing comes from the data path
        // (contents_changed) above. We deliberately do NOT pin the scroll here:
        // pinning from `changed` reacts to virtualization's own `upper` changes
        // (hidden off-screen blocks collapse to 0 height) and oscillates forever.
        // The follow-bottom pin is the deferred idle scheduled on contents_changed.
        {
            let f = layout_active_surface.clone();
            let last_page = Rc::new(Cell::new(0.0f64));
            block_scroll.vadjustment().connect_changed(move |adj| {
                let page = adj.page_size();
                if (page - last_page.get()).abs() > 0.5 {
                    last_page.set(page);
                    f();
                }
            });
        }

        // ── Jump-to-bottom FAB click: return to the live prompt ───────────
        {
            let scroll = block_scroll.clone();
            let programmatic = programmatic_scroll.clone();
            let user_scrolled = user_scrolled_up.clone();
            let unread = unread_count.clone();
            let vte_for_fab = active_vte.downgrade();
            jump_fab.connect_clicked(move |button| {
                // Returning to the live prompt is not a single set_value: blocks
                // below the viewport are virtualized to 0 height, so `upper` only
                // grows as they scroll into view. One jump lands partway; we have
                // to re-apply `upper - page` across idle passes until `upper` stops
                // growing (true bottom reached) or we hit a small iteration cap.
                user_scrolled.set(false);
                unread.set(0);
                button.set_visible(false);
                // "Jump to latest" also means the latest of the live buffer:
                // leave any scrolled-up position inside the running output.
                if let Some(adj) = vte_for_fab.upgrade().and_then(|vte| vte.vadjustment()) {
                    adj.set_value((adj.upper() - adj.page_size()).max(adj.lower()));
                }
                let adj = scroll.vadjustment();
                programmatic.set(true);
                adj.set_value((adj.upper() - adj.page_size()).max(adj.lower()));
                programmatic.set(false);

                let scroll = scroll.clone();
                let programmatic = programmatic.clone();
                let tries = Rc::new(Cell::new(0u8));
                let stable_turns = Rc::new(Cell::new(0u8));
                let last_target = Rc::new(Cell::new(None::<f64>));
                glib::timeout_add_local(std::time::Duration::from_millis(16), move || {
                    // Run on successive frames so virtualized blocks have time to
                    // remap and grow `upper`. Position stability alone is not
                    // enough: the current target can be reached before the true
                    // bottom geometry has appeared.
                    tries.set(tries.get().saturating_add(1));
                    let adj = scroll.vadjustment();
                    let target = (adj.upper() - adj.page_size()).max(adj.lower());
                    programmatic.set(true);
                    adj.set_value(target);
                    programmatic.set(false);

                    let target_is_stable = last_target
                        .get()
                        .is_some_and(|previous| (previous - target).abs() < 1.0);
                    last_target.set(Some(target));
                    if target_is_stable && (adj.value() - target).abs() < 1.0 {
                        stable_turns.set(stable_turns.get().saturating_add(1));
                    } else {
                        stable_turns.set(0);
                    }

                    if stable_turns.get() >= 2 || tries.get() >= 12 {
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                });
            });
        }

        // ── Sticky command header ────────────────────────────────────────
        // Running commands keep their status header; oversized finished blocks
        // pin their command after the original header scrolls above the viewport.
        {
            let pty_for_stop = pty.clone();
            let hold_for_stop = selection_feed_hold.clone();
            sticky_stop_btn.connect_clicked(move |_| {
                // Resume a parked feed first so the ^C echo and the command's
                // shutdown output are visible immediately.
                hold_for_stop.flush_now();
                if let Err(error) = pty_for_stop.write_bytes(b"\x03") {
                    pty_for_stop.report_write_error("could not queue interrupt", error);
                }
            });
        }
        let sticky_timer_id = {
            let sticky = sticky_bar.clone();
            let sticky_label = sticky_label.clone();
            let sticky_jump_bottom = sticky_jump_bottom_btn.clone();
            let sticky_stop = sticky_stop_btn.clone();
            let sticky_target = sticky_target_id.clone();
            let sticky_minimized = sticky_minimized.clone();
            let cmd_running = cmd_running.clone();
            let running_cmd = running_cmd.clone();
            let block_start_time = block_start_time.clone();
            let user_scrolled = user_scrolled_up.clone();
            let finished = finished_blocks_rc.clone();
            let scroll = block_scroll.clone();
            let fullscreen = fullscreen.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                if sticky.parent().is_none() {
                    return glib::ControlFlow::Break;
                }
                let minimized = sticky_minimized.get();
                if fullscreen.get() {
                    sticky_target.set(None);
                    sticky_jump_bottom.set_visible(false);
                    sticky_stop.set_visible(false);
                    sticky.set_visible(false);
                    return glib::ControlFlow::Continue;
                }
                if !user_scrolled.get() {
                    sticky_target.set(None);
                    sticky_jump_bottom.set_visible(false);
                    sticky_stop.set_visible(false);
                    sticky.set_visible(false);
                    return glib::ControlFlow::Continue;
                }
                if cmd_running.get() {
                    sticky_target.set(None);
                    sticky_jump_bottom.set_visible(false);
                    sticky_stop.set_visible(!minimized);
                    let cmd = running_cmd.borrow();
                    let cmd_disp = cmd.trim();
                    let elapsed = block_start_time
                        .get()
                        .and_then(|st| SystemTime::now().duration_since(st).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let elapsed_str = if elapsed >= 3600 {
                        format!("{}h{:02}m", elapsed / 3600, (elapsed % 3600) / 60)
                    } else if elapsed >= 60 {
                        format!("{}m{:02}s", elapsed / 60, elapsed % 60)
                    } else {
                        format!("{}s", elapsed)
                    };
                    let label = if cmd_disp.is_empty() {
                        format!("\u{25b6}  (running)    {}", elapsed_str)
                    } else {
                        format!("\u{25b6}  {}    {}", cmd_disp, elapsed_str)
                    };
                    sticky_label.set_text(&label);
                    sticky_label.set_visible(!minimized);
                    sticky.set_visible(true);
                    return glib::ControlFlow::Continue;
                }
                let sticky_height = sticky.height().max(1) as f32;
                let candidate = finished.borrow().iter().find_map(|block| {
                    let header = block.header_row.compute_bounds(&scroll)?;
                    let card = block.widget().compute_bounds(&scroll)?;
                    let header_bottom = header.y() + header.height();
                    let card_bottom = card.y() + card.height();
                    if header_bottom <= 0.0 && card_bottom > sticky_height + 4.0 {
                        let command = block
                            .cmd_text
                            .lines()
                            .next()
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        Some((block.id, command, block.long_output))
                    } else {
                        None
                    }
                });
                if let Some((id, command, long_output)) = candidate {
                    sticky_target.set(Some(id));
                    let command = if command.is_empty() {
                        "Background output".to_string()
                    } else {
                        command
                    };
                    let command = crate::review_input::safe_inline_display(&command, 512);
                    sticky_label.set_text(&format!("\u{276f}  {command}"));
                    sticky_label.set_visible(!minimized);
                    sticky_jump_bottom.set_visible(!minimized && long_output);
                    sticky_stop.set_visible(false);
                    sticky.set_visible(true);
                } else {
                    sticky_target.set(None);
                    sticky_jump_bottom.set_visible(false);
                    sticky_stop.set_visible(false);
                    sticky.set_visible(false);
                }
                glib::ControlFlow::Continue
            })
        };

        // ── VTE is used as a display-only widget (fed via feed() in alt-screen mode)
        //    so we do NOT attach it to the PTY. Our reader thread handles all I/O.

        // ── Live VTE input → PTY (jterm1 model) ───────────────────────────
        // The active VTE has input_enabled(true), so it translates keystrokes and
        // owns IME natively; its `commit` signal carries the bytes to send. We
        // forward them to the PTY and, while awaiting a command, reconstruct the
        // typed command line so the finalize path can style it into the block.
        {
            let pty_for_commit = pty.clone();
            let bstate_for_commit = bstate.clone();
            let typed_cmd_for_commit = typed_cmd.clone();
            let idle_input_dirty_for_commit = idle_input_dirty.clone();
            let pty_synced_for_commit = pty_synced.clone();
            let finished_blocks_for_commit = Rc::downgrade(&finished_blocks_rc);
            let selected_block_ids_for_commit = selected_block_ids.clone();
            let selected_block_id_for_commit = selected_block_id.clone();
            let selection_anchor_id_for_commit = selection_anchor_id.clone();
            active_vte.connect_commit(move |_, text, _size| {
                let awaiting_command = bstate_for_commit.get() == BlockState::AwaitingCommand;
                let shadow_rollback = awaiting_command
                    .then(|| vte_commit_shadow_rollback(&typed_cmd_for_commit.borrow(), text));
                let previous_idle_dirty = idle_input_dirty_for_commit.get();
                let previous_pty_synced = pty_synced_for_commit.get();
                if awaiting_command {
                    idle_input_dirty_for_commit.set(true);
                    if text.as_bytes().iter().any(|&b| b != b'\r' && b != b'\n') {
                        // A later recall must replace this edited readline buffer,
                        // not append to it. PromptEnd resets the flag for a new line.
                        pty_synced_for_commit.set(true);
                    }

                    // Update the fallback before exposing bytes to the PTY. A very
                    // fast shell can echo the line and emit OSC 133;C immediately;
                    // the reader must never observe CommandStart while this shadow
                    // still describes the previous editor state.
                    apply_vte_commit_to_shadow(&mut typed_cmd_for_commit.borrow_mut(), text);
                }

                if let Err(error) = pty_for_commit.write_bytes(text.as_bytes()) {
                    if let Some(shadow_rollback) = shadow_rollback {
                        shadow_rollback.apply(&mut typed_cmd_for_commit.borrow_mut());
                        idle_input_dirty_for_commit.set(previous_idle_dirty);
                        pty_synced_for_commit.set(previous_pty_synced);
                    }
                    pty_for_commit.report_write_error("could not queue terminal input", error);
                    return;
                }

                // Only accepted terminal input exits block-selection mode.
                // Otherwise a saturated queue would mutate UI/editor state
                // even though the shell never received the keystroke.
                if selected_block_id_for_commit.get().is_some() {
                    if let Some(finished_blocks_for_commit) = finished_blocks_for_commit.upgrade() {
                        let finished = finished_blocks_for_commit.borrow();
                        clear_finished_block_selection(
                            &finished,
                            &selected_block_ids_for_commit,
                            &selected_block_id_for_commit,
                            &selection_anchor_id_for_commit,
                        );
                    }
                }
            });
        }

        // While a normal command is running, the active VTE is still the live
        // terminal surface. Let it own printable keys, Enter, Backspace, control
        // sequences, and IME preedit/commit. This root capture handler is only a
        // focus fallback for interrupt/EOF; forwarding text here would bypass
        // GTK's input method context and break CJK composition.
        {
            let pty_for_root_key = pty.clone();
            let bstate_for_root_key = bstate.clone();
            let root_key = gtk4::EventControllerKey::new();
            root_key.set_propagation_phase(gtk4::PropagationPhase::Capture);
            root_key.connect_key_pressed(move |_controller, keyval, _keycode, modifiers| {
                if !matches!(
                    bstate_for_root_key.get(),
                    BlockState::CollectingOutput | BlockState::PostCommand
                ) {
                    return glib::Propagation::Proceed;
                }

                if let Some(bytes) = running_root_control_bytes(keyval, modifiers) {
                    if let Err(error) = pty_for_root_key.write_bytes(bytes) {
                        pty_for_root_key
                            .report_write_error("could not queue process-control key", error);
                    }
                    return glib::Propagation::Stop;
                }

                glib::Propagation::Proceed
            });
            root.add_controller(root_key);
        }

        // ── Keyboard navigation / copy-paste (Capture phase) ──────────────
        {
            let pty_for_key = pty.clone();
            let typed_cmd_for_key = typed_cmd.clone();
            let finished_blocks_for_key = Rc::downgrade(&finished_blocks_rc);
            let block_data_for_key = block_data_rc.clone();
            let block_list_for_key = block_list.clone();
            let selected_block_ids_for_key = selected_block_ids.clone();
            let selected_block_id_for_key = selected_block_id.clone();
            let selection_anchor_id_for_key = selection_anchor_id.clone();
            let block_scroll_for_key = block_scroll.clone();
            let key_ctrl = gtk4::EventControllerKey::new();
            key_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);

            KeyCtx {
                pty_for_key,
                active_vte_for_key: active_vte.downgrade(),
                pty_synced_for_key: pty_synced.clone(),
                bracketed_paste_for_key: bracketed_paste.clone(),
                typed_cmd_for_key,
                finished_blocks_for_key,
                block_data_for_key,
                block_list_for_key: block_list_for_key.downgrade(),
                selected_block_ids_for_key,
                selected_block_id_for_key,
                selection_anchor_id_for_key,
                block_scroll_for_key: block_scroll_for_key.downgrade(),
                bookmarks_for_key: block_bookmarks.clone(),
                visible_indices_for_key: visible_indices.clone(),
                bstate_for_key: bstate.clone(),
            }
            .connect(&key_ctrl);

            active_vte.add_controller(key_ctrl);
        }

        // Clicking back into the live prompt is an explicit exit from historical
        // block selection. Programmatic focus from a header click does not trigger
        // this gesture, so keyboard block navigation remains intact.
        {
            let finished_for_click = Rc::downgrade(&finished_blocks_rc);
            let selected_ids_for_click = selected_block_ids.clone();
            let selected_for_click = selected_block_id.clone();
            let anchor_for_click = selection_anchor_id.clone();
            let active_click = gtk4::GestureClick::new();
            active_click.set_button(1);
            active_click.set_propagation_phase(gtk4::PropagationPhase::Capture);
            active_click.connect_pressed(move |_, _, _, _| {
                if selected_for_click.get().is_some() {
                    if let Some(finished_for_click) = finished_for_click.upgrade() {
                        let finished = finished_for_click.borrow();
                        clear_finished_block_selection(
                            &finished,
                            &selected_ids_for_click,
                            &selected_for_click,
                            &anchor_for_click,
                        );
                    }
                }
            });
            active_vte.add_controller(active_click);
        }

        // Wheel handling inside an alt-screen + mouse-reporting app (less / vim /
        // htop). VTE only synthesizes mouse-wheel CSI sequences when it owns the
        // PTY; ours is fed by our reader, so we synthesize and write the bytes
        // ourselves. The pointer cell under the cursor is tracked via a motion
        // controller so the column/row in the report matches what the user sees.
        //
        // - alt-screen + mouse mode + scroll_reporting_enabled → encode wheel,
        //   write to PTY, stop propagation (so block_scroll doesn't also scroll).
        // - alt-screen + mouse mode + !scroll_reporting_enabled → swallow wheel
        //   (user has opted out of mouse-driven paging).
        // - otherwise → let the event bubble to block_scroll for normal scroll.
        {
            // Track pointer position over the live VTE in cell coordinates so
            // wheel events emitted below can include accurate col/row.
            let pointer_cell: Rc<Cell<(i64, i64)>> = Rc::new(Cell::new((1, 1)));
            {
                let pointer_for_motion = pointer_cell.clone();
                let vte_for_motion = active_vte.downgrade();
                let motion = gtk4::EventControllerMotion::new();
                motion.set_propagation_phase(gtk4::PropagationPhase::Capture);
                motion.connect_motion(move |_, x, y| {
                    let Some(vte_for_motion) = vte_for_motion.upgrade() else {
                        return;
                    };
                    let cw = (vte_for_motion.char_width() as f64).max(1.0);
                    let ch = (vte_for_motion.char_height() as f64).max(1.0);
                    let col = (x / cw).floor() as i64 + 1;
                    let row = (y / ch).floor() as i64 + 1;
                    pointer_for_motion.set((col.max(1), row.max(1)));
                });
                active_vte.add_controller(motion);
            }

            let fullscreen_for_scroll = fullscreen.clone();
            let mouse_mode_for_scroll = mouse_reporting_mode.clone();
            let scroll_enabled = config.scroll_reporting_enabled;
            let pty_for_scroll = pty.clone();
            let pointer_for_scroll = pointer_cell.clone();
            let bstate_for_scroll = bstate.clone();
            let vte_for_scroll = active_vte.downgrade();
            let outer_for_scroll = block_scroll.downgrade();
            let scroll_ctrl = gtk4::EventControllerScroll::new(
                gtk4::EventControllerScrollFlags::VERTICAL
                    | gtk4::EventControllerScrollFlags::HORIZONTAL,
            );
            scroll_ctrl.set_propagation_phase(gtk4::PropagationPhase::Capture);
            scroll_ctrl.connect_scroll(move |_, _dx, dy| {
                let in_mouse_app = fullscreen_for_scroll.get()
                    && mouse_mode_for_scroll.get() != MouseReportingMode::None;
                if in_mouse_app {
                    if !scroll_enabled {
                        return glib::Propagation::Stop;
                    }
                    let (col, row) = pointer_for_scroll.get();
                    if let Some(bytes) =
                        encode_mouse_wheel(mouse_mode_for_scroll.get(), dy, col, row)
                    {
                        if let Err(error) = pty_for_scroll.write_bytes(&bytes) {
                            pty_for_scroll
                                .report_write_error("could not queue mouse-wheel input", error);
                        }
                    }
                    return glib::Propagation::Stop;
                }
                // Alt-screen without mouse reporting: VTE natively fakes
                // arrow keys for the wheel (less/vim paging). Let it.
                if bstate_for_scroll.get() == BlockState::AltScreen {
                    return glib::Propagation::Proceed;
                }
                // While a command streams, its scrollback is a first-class
                // reading surface: the wheel scrolls the live VTE itself and
                // hands off to the outer history only at the buffer's edge.
                if matches!(
                    bstate_for_scroll.get(),
                    BlockState::CollectingOutput
                        | BlockState::PostCommand
                        | BlockState::RawFallback
                ) {
                    if let Some(adj) = vte_for_scroll.upgrade().and_then(|vte| vte.vadjustment()) {
                        if scroll_adjustment_by_wheel(&adj, dy) {
                            return glib::Propagation::Stop;
                        }
                    }
                }
                // Prompt states, and streaming edges, scroll the block history.
                // Never Proceed here: VTE's fallback scrolling swallows every
                // wheel it receives (even with nothing left to scroll), which
                // used to make the wheel dead over the idle prompt cell.
                let Some(outer) = outer_for_scroll.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                forward_outer_scroll(&outer, dy);
                glib::Propagation::Stop
            });
            active_vte.add_controller(scroll_ctrl);

            // Wheeling over the live scrollbar mirrors the VTE surface: move
            // the live buffer, hand off to the history at its edges. Without
            // the capture the GtkScrollbar scrolls natively but sticks dead
            // at the ends (same trap the finished-block scrollbar closes).
            let live_scrollbar = active.borrow().live_scrollbar.clone();
            let scrollbar_scroll =
                gtk4::EventControllerScroll::new(gtk4::EventControllerScrollFlags::VERTICAL);
            scrollbar_scroll.set_propagation_phase(gtk4::PropagationPhase::Capture);
            let vte_for_scrollbar = active_vte.downgrade();
            let outer_for_scrollbar = block_scroll.downgrade();
            scrollbar_scroll.connect_scroll(move |_, _dx, dy| {
                if let Some(adj) = vte_for_scrollbar
                    .upgrade()
                    .and_then(|vte| vte.vadjustment())
                {
                    if scroll_adjustment_by_wheel(&adj, dy) {
                        return glib::Propagation::Stop;
                    }
                }
                let Some(outer) = outer_for_scrollbar.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                forward_outer_scroll(&outer, dy);
                glib::Propagation::Stop
            });
            live_scrollbar.add_controller(scrollbar_scroll);
        }

        let cross_selection = CrossSelection::install(
            &block_scroll,
            finished_blocks_rc.clone(),
            active_vte.clone(),
            selected_block_ids.clone(),
            selected_block_id.clone(),
            selection_anchor_id.clone(),
            selection_feed_hold.clone(),
            bstate.clone(),
            mouse_reporting_mode.clone(),
        );

        let term_view = TermView {
            root,
            pane_header,
            block_scroll,
            block_list,
            jump_fab: jump_fab.clone(),
            unread_count: unread_count.clone(),
            active_vte,
            active,
            bstate,
            prompt_buf,
            typed_cmd,
            external_submission,
            idle_input_dirty,
            fullscreen,
            user_scrolled_up: user_scrolled_up.clone(),
            programmatic_scroll: programmatic_scroll.clone(),
            scroll_debouncer,
            pty,
            pty_synced: pty_synced.clone(),
            cwd_callbacks,
            remote_session_callbacks,
            exited_callbacks,
            bell_callbacks,
            title_callbacks,
            activity_callbacks,
            mouse_reporting_mode,
            bracketed_paste,
            config: Rc::new(RefCell::new(config.clone())),
            block_data: block_data_rc,
            finished_blocks: finished_blocks_rc,
            widget_pool: widget_pool.clone(),
            viewport: Rc::new(RefCell::new(ViewportState {
                first_visible: 0,
                last_visible: 0,
                total_height: 0,
            })),
            visible_indices,
            selected_block_ids,
            selected_block_id,
            selection_anchor_id,
            bookmarks: block_bookmarks,
            find_state: Rc::new(RefCell::new(FindState::default())),
            current_cwd: current_cwd.clone(),
            session_id: session_id_owned,
            history_load: Arc::new(history::HistoryLoadShared::default()),
            persist_history_on_drop: Cell::new(true),
            history_load_poll_id: RefCell::new(None),
            resize_tick_id: RefCell::new(None),
            sticky_timer_id: RefCell::new(Some(sticky_timer_id)),
            cross_selection,
            block_finished_callbacks,
            selection_feed_hold,
        };

        // Before the first map GTK has no real page_size. In that case
        // update_viewport leaves the conservative initial range (block 0)
        // untouched, preventing every restored snapshot VTE from mapping at
        // once. The map handler below replaces it with the first valid range.
        term_view.update_viewport();
        term_view.update_block_visibility();

        // Wire virtual scrolling. Scroll-value changes and viewport-size range
        // changes affect which cards intersect the viewport; upper-only range
        // changes are a visibility side effect and are ignored below. Map is the
        // reliable recovery point after a Notebook page temporarily reports
        // zero-sized geometry.
        {
            let viewport = term_view.viewport.clone();
            let block_scroll = term_view.block_scroll.clone();
            let block_data = term_view.block_data.clone();
            let config = term_view.config.clone();
            let finished_blocks = Rc::downgrade(&term_view.finished_blocks);
            let visible_indices = term_view.visible_indices.clone();
            let fullscreen = term_view.fullscreen.clone();
            let visibility_update_pending = Rc::new(Cell::new(false));
            let block_scroll_weak = block_scroll.downgrade();
            let last_page_size = Rc::new(Cell::new(None::<f64>));

            let schedule_visibility_update: Rc<dyn Fn()> = Rc::new(move || {
                let Some(block_scroll) = block_scroll_weak.upgrade() else {
                    return;
                };
                if fullscreen.get()
                    || !block_scroll.is_mapped()
                    || visibility_update_pending.replace(true)
                {
                    return;
                }
                let Some(finished_blocks) = finished_blocks.upgrade() else {
                    visibility_update_pending.set(false);
                    return;
                };

                let vp = viewport.clone();
                let scroll = block_scroll.clone();
                let finished = finished_blocks.clone();
                let block_data = block_data.clone();
                let config = config.clone();
                let visible = visible_indices.clone();
                let fullscreen = fullscreen.clone();
                let pending = visibility_update_pending.clone();
                glib::idle_add_local_once(move || {
                    pending.set(false);
                    if fullscreen.get() || !scroll.is_mapped() {
                        return;
                    }

                    // Re-read geometry in the idle instead of applying a value
                    // captured during switch-page. Mapping/allocation may have
                    // completed between the signal and this callback.
                    let adj = scroll.vadjustment();
                    let margin = config.borrow().virtual_scroll_margin;
                    let block_data_ref = block_data.borrow();
                    let Some(next_viewport) = viewport_state_for_scroll(
                        &block_data_ref,
                        adj.value(),
                        adj.page_size(),
                        margin,
                    ) else {
                        return;
                    };
                    // One extra margin page of hysteresis: see
                    // `stable_visible_indices`.
                    let loose_viewport = viewport_state_for_scroll(
                        &block_data_ref,
                        adj.value(),
                        adj.page_size(),
                        margin.saturating_add(1),
                    );
                    drop(block_data_ref);

                    let new_visible = stable_visible_indices(
                        &next_viewport,
                        loose_viewport.as_ref(),
                        &visible.borrow(),
                    );
                    *vp.borrow_mut() = next_viewport;

                    let finished_ref = finished.borrow();
                    let mut block_data_ref = block_data.borrow_mut();
                    let mut visible_ref = visible.borrow_mut();
                    apply_visible_indices(
                        &finished_ref,
                        &mut block_data_ref,
                        &mut visible_ref,
                        new_visible,
                    );
                });
            });

            let vadjust = term_view.block_scroll.vadjustment();
            {
                let schedule = schedule_visibility_update.clone();
                let last_page_size = last_page_size.clone();
                vadjust.connect_changed(move |adj| {
                    if viewport_page_size_changed(&last_page_size, adj.page_size()) {
                        schedule();
                    }
                });
            }
            {
                let schedule = schedule_visibility_update.clone();
                vadjust.connect_value_changed(move |_| schedule());
            }
            term_view
                .block_scroll
                .connect_map(move |_| schedule_visibility_update());
        }

        // ── Resize handler: sync PTY cols/rows when widget allocation changes ──
        term_view.install_resize_tick();

        Ok(term_view)
    }

    /// Keep PTY geometry synchronized with the real pane viewport, independent
    /// of the compact/full visual state of the live VTE. FTCS transitions also
    /// push TIOCSWINSZ synchronously so apps never see a stale first layout.
    fn install_resize_tick(&self) {
        let pty_for_resize = self.pty.clone();
        let scroll_for_resize = self.block_scroll.downgrade();
        let last: Rc<Cell<(u16, u16)>> = Rc::new(Cell::new((0, 0)));
        let tick_id = self.active_vte.add_tick_callback(move |vte, _clock| {
            let Some(scroll_for_resize) = scroll_for_resize.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let (cols, rows) = pty_grid_size(vte, &scroll_for_resize);
            if cols > 0 && rows > 0 && (cols, rows) != last.get() {
                last.set((cols, rows));
                pty_for_resize.resize(cols, rows);
            }
            glib::ControlFlow::Continue
        });
        *self.resize_tick_id.borrow_mut() = Some(tick_id);
    }

    /// Root GTK widget to embed in the notebook page.
    pub(crate) fn pane_header(&self) -> &crate::ui::PaneHeader {
        &self.pane_header
    }

    pub fn widget(&self) -> gtk4::Widget {
        self.root.clone().upcast()
    }

    fn clear_block_selection_for_input(&self) {
        if self.selected_block_id.get().is_none() {
            return;
        }
        let finished = self.finished_blocks.borrow();
        clear_finished_block_selection(
            &finished,
            &self.selected_block_ids,
            &self.selected_block_id,
            &self.selection_anchor_id,
        );
    }

    /// Send key bytes into the PTY (user input).
    #[must_use = "terminal input may be rejected by bounded nonblocking backpressure"]
    pub fn write_input(&self, data: &[u8]) -> Result<(), crate::pty::PtyWriteError> {
        // Input while the feed is parked for a selection reads as a hang;
        // resume before the echo of these bytes would be parked too.
        self.selection_feed_hold.flush_now();
        let previous_shadow = self.typed_cmd.borrow().clone();
        let previous_pty_synced = self.pty_synced.get();
        let previous_idle_dirty = self.idle_input_dirty.get();
        let changed_editor = record_external_input(
            self.bstate.get(),
            data,
            &self.typed_cmd,
            &self.pty_synced,
            &self.idle_input_dirty,
        );
        if let Err(error) = self.pty.write_bytes(data) {
            *self.typed_cmd.borrow_mut() = previous_shadow;
            self.pty_synced.set(previous_pty_synced);
            self.idle_input_dirty.set(previous_idle_dirty);
            return Err(error);
        }
        if changed_editor && self.selected_block_id.get().is_some() {
            let finished = self.finished_blocks.borrow();
            clear_finished_block_selection(
                &finished,
                &self.selected_block_ids,
                &self.selected_block_id,
                &self.selection_anchor_id,
            );
        }
        Ok(())
    }

    /// Submit the current shell edit buffer as if the user pressed Enter.
    ///
    /// A terminal Enter key is carriage return, not line feed. The PTY input
    /// sanitizer deliberately treats LF as insertion-only multiline content,
    /// so programmatic execution paths must use this explicit submission API.
    pub fn submit_input(&self) -> Result<(), crate::pty::PtyWriteError> {
        self.write_input(b"\r")
    }

    /// Write and submit one already-approved programmatic command.
    ///
    /// Save the exact text before exposing bytes to the PTY so a fast shell
    /// cannot emit CommandStart before the Block command capture is armed.
    pub fn submit_command(&self, command: &str) -> Result<(), String> {
        let submission = approved_command_submission_payload(command)?;
        let previous_submission = self.external_submission.borrow().clone();
        *self.external_submission.borrow_mut() = Some(command.to_string());
        if let Err(error) = self.write_input(&submission) {
            *self.external_submission.borrow_mut() = previous_submission;
            return Err(error.to_string());
        }
        Ok(())
    }

    /// Insert a transient notice card (e.g. an AI command-correction proposal
    /// or the Shell Agent session card) into the block list just above the live
    /// prompt, so it reads like part of the block conversation. The card is not
    /// a `FinishedBlock`: it never joins `finished_blocks`/`block_data`, so
    /// virtualization, selection, and history persistence all ignore it.
    ///
    /// Calling this again for a widget already in the block list re-pins it
    /// directly above the prompt (used after a finished block lands below it).
    pub fn insert_inline_notice(&self, widget: &gtk4::Widget) {
        let active_widget = self.active.borrow().widget().clone();
        let already_inserted = widget
            .parent()
            .is_some_and(|parent| parent == *self.block_list.upcast_ref::<gtk4::Widget>());
        if already_inserted {
            let anchor = active_widget.prev_sibling();
            if anchor.as_ref() != Some(widget) {
                self.block_list.reorder_child_after(widget, anchor.as_ref());
            }
        } else {
            widget.insert_before(&self.block_list, Some(&active_widget));
        }
        self.block_list.queue_allocate();
        self.scroll_debouncer
            .pin_to_bottom_deferred(&self.block_scroll);
    }

    /// Remove a card previously added by `insert_inline_notice`. Safe to call
    /// twice: removal is skipped when the widget is no longer in the block list.
    pub fn remove_inline_notice(&self, widget: &gtk4::Widget) {
        if widget
            .parent()
            .is_some_and(|parent| parent == *self.block_list.upcast_ref::<gtk4::Widget>())
        {
            self.block_list.remove(widget);
            self.block_list.queue_allocate();
        }
    }

    /// Review-gated commands may only be inserted or submitted into a clean,
    /// idle shell editor. The status is intentionally diagnostic so callers can
    /// distinguish a running command from stale input or missing integration.
    pub(crate) fn command_prompt_status(&self) -> CommandPromptStatus {
        classify_command_prompt_status(
            self.bstate.get(),
            self.fullscreen.get(),
            self.idle_input_dirty.get(),
            self.pty_synced.get(),
            self.typed_cmd.borrow().trim().is_empty(),
        )
    }

    pub fn can_accept_agent_command(&self) -> bool {
        self.command_prompt_status().is_ready()
    }

    /// Resize the PTY.
    pub fn resize(&self, cols: u16, rows: u16) {
        self.pty.resize(cols, rows);
    }

    /// Kill the child process.
    pub fn kill(&self) {
        self.pty.kill();
    }

    pub fn pid_i32(&self) -> i32 {
        self.pty.pid_i32()
    }

    /// Borrow the real master-side PTY descriptor for foreground-process
    /// probing.  Block mode does not attach its custom PTY to VTE's `pty()`
    /// property, so callers must use this descriptor instead.
    pub fn pty_fd_i32(&self) -> i32 {
        self.pty.master_fd_raw()
    }

    pub fn vte(&self) -> &Terminal {
        &self.active_vte
    }

    pub fn cwd(&self) -> String {
        self.current_cwd.borrow().clone()
    }

    pub fn grab_focus(&self) {
        focus_terminal(&self.active_vte);
    }

    /// Copy selected text to clipboard.
    ///
    /// Visible text selection wins over card selection. This matters when the
    /// user selects a block for navigation, then drags a smaller range inside its
    /// command/output VTE: Ctrl+Shift+C must copy the highlighted text they see,
    /// not the entire card.
    pub fn copy_to_clipboard(&self) {
        self.copy_to_clipboard_with_modifier(false);
    }

    /// Same as `copy_to_clipboard` but also honors the Warp "copy block output
    /// only" modifier (Alt+Ctrl+Shift+C) when a whole block is selected.
    pub fn copy_to_clipboard_with_modifier(&self, alt_held: bool) {
        log::debug!(">>> TermView::copy_to_clipboard called (alt={})", alt_held);

        // Native and cross-block text selections are collected in document order:
        // command VTE, output VTE, then the live input surface.
        if let Some(text) = self.cross_selection.copy_text() {
            log::debug!(
                ">>> TermView copy: got {} chars from visible text selection",
                text.len()
            );
            self.active_vte.clipboard().set_text(&text);
            // The selection is captured; resume any feed parked to keep it
            // alive while the command kept streaming.
            self.selection_feed_hold.flush_now();
            return;
        }

        // Whole-block selection (Warp's CopyBlock; +Alt -> output only).
        // Multi-selection preserves terminal order and visual grouping.
        {
            let selected = self.selected_block_ids.borrow();
            if !selected.is_empty() {
                let data = self.block_data.borrow();
                let parts: Vec<String> = data
                    .iter()
                    .filter(|block| selected.contains(&block.id))
                    .map(|block| block_clipboard_text(&block.cmd, &block.output, alt_held))
                    .collect();
                if !parts.is_empty() {
                    let text = parts.join("\n\n");
                    log::debug!(
                        ">>> TermView copy: copied {} selected blocks ({} chars)",
                        parts.len(),
                        text.len()
                    );
                    self.active_vte.clipboard().set_text(&text);
                    return;
                }
            }
        }

        // No visible selection. Do not fall back to PRIMARY: it is unreliable on
        // Wayland and makes the shortcut copy content the user cannot see selected.
        log::debug!(">>> TermView copy: no selection found, nothing to copy");
    }

    /// Paste clipboard text as one ordered write to block mode's shell PTY.
    ///
    /// The active VTE is display-only in this mode and has no child PTY, so
    /// `Terminal::paste_clipboard()` can lose or reorder multiline input. Read
    /// the clipboard ourselves, update the shared editor guards, and preserve
    /// bracketed-paste framing in one queued PTY write.
    pub fn paste_from_clipboard(&self) {
        // Pasting is an explicit return to the live editor. Without clearing the
        // card selection, the next Enter is intercepted as “recall selected
        // command” instead of submitting the pasted text.
        self.selection_feed_hold.flush_now();
        self.clear_block_selection_for_input();
        self.active.borrow().grab_focus();

        let clipboard = self.active_vte.clipboard();
        let pty = self.pty.clone();
        let bracketed_paste = self.bracketed_paste.clone();
        let bstate = self.bstate.clone();
        let typed_cmd = self.typed_cmd.clone();
        let pty_synced = self.pty_synced.clone();
        let idle_input_dirty = self.idle_input_dirty.clone();
        let finished_blocks = self.finished_blocks.clone();
        let selected_block_ids = self.selected_block_ids.clone();
        let selected_block_id = self.selected_block_id.clone();
        let selection_anchor_id = self.selection_anchor_id.clone();
        let active = self.active.clone();
        clipboard.read_text_async(None::<&gtk4::gio::Cancellable>, move |result| {
            let Ok(Some(text)) = result else {
                return;
            };
            let text = text.to_string();
            if text.is_empty() {
                return;
            }

            let paste = build_clipboard_paste(&text, bracketed_paste.get());
            if paste.is_empty() {
                return;
            }
            if paste.risk.had_embedded_paste_marker {
                // The clipboard tried to close the paste frame early so its
                // remainder would arrive as a command line. Already defused —
                // record it, because it is not something a user does by accident.
                log::warn!("removed bracketed-paste markers from a pasted clipboard payload");
            }

            let previous_shadow = typed_cmd.borrow().clone();
            let previous_pty_synced = pty_synced.get();
            let previous_idle_dirty = idle_input_dirty.get();
            record_external_input(
                bstate.get(),
                paste.echo_text.as_bytes(),
                &typed_cmd,
                &pty_synced,
                &idle_input_dirty,
            );
            if let Err(error) = pty.write_bytes(&paste.bytes) {
                *typed_cmd.borrow_mut() = previous_shadow;
                pty_synced.set(previous_pty_synced);
                idle_input_dirty.set(previous_idle_dirty);
                pty.report_write_error("could not queue clipboard paste", error);
                return;
            }
            if selected_block_id.get().is_some() {
                let finished = finished_blocks.borrow();
                clear_finished_block_selection(
                    &finished,
                    &selected_block_ids,
                    &selected_block_id,
                    &selection_anchor_id,
                );
            }
            active.borrow().grab_focus();
        });
    }

    pub fn connect_cwd_changed<F: Fn(&str) + 'static>(&self, f: F) {
        self.cwd_callbacks.borrow_mut().push(Box::new(f));
    }

    pub fn connect_remote_session_id<F: Fn(&str) + 'static>(&self, f: F) {
        self.remote_session_callbacks.borrow_mut().push(Box::new(f));
    }

    pub fn connect_exited<F: Fn(i32) + 'static>(&self, f: F) {
        self.exited_callbacks.borrow_mut().push(Box::new(f));
    }

    pub fn connect_bell<F: Fn() + 'static>(&self, f: F) {
        self.bell_callbacks.borrow_mut().push(Box::new(f));
    }

    pub fn connect_title_changed<F: Fn(&str) + 'static>(&self, f: F) {
        self.title_callbacks.borrow_mut().push(Box::new(f));
    }

    pub fn connect_activity<F: Fn() + 'static>(&self, f: F) {
        self.activity_callbacks.borrow_mut().push(Box::new(f));
    }

    pub fn connect_block_finished<F>(&self, f: F)
    where
        F: Fn(String, Option<i32>, String, Option<u64>) + 'static,
    {
        self.block_finished_callbacks.borrow_mut().push(Box::new(f));
    }

    /// Reveal the live input when its tab becomes active.
    ///
    /// This deliberately reuses the same frame-spaced, generation-aware bottom
    /// pin as output finalization. The old activation-only idle loop could spend
    /// all twelve retries before GTK produced another allocation, so a newly
    /// selected tab sometimes stopped above its bottom input block.
    pub(crate) fn reveal_live_input(&self) {
        self.scroll_debouncer.reset_scroll_lock();
        self.unread_count.set(0);
        set_jump_fab_label(&self.jump_fab, 0);
        self.jump_fab.set_visible(false);
        self.block_list.queue_allocate();
        self.scroll_debouncer
            .pin_to_bottom_deferred(&self.block_scroll);
    }

    pub fn scroll_lines(&self, lines: i32) {
        // Ctrl+Up enters jterm1/Warp-style block selection at the newest block.
        {
            let finished = self.finished_blocks.borrow();
            if (lines < 0 || self.selected_block_id.get().is_some())
                && move_finished_block_selection(
                    &finished,
                    &self.selected_block_ids,
                    &self.selected_block_id,
                    &self.selection_anchor_id,
                    &self.block_scroll,
                    lines.signum(),
                )
            {
                self.cross_selection.clear_all();
                return;
            }
        }

        let adj = self.block_scroll.vadjustment();
        let cell_h = self.active_vte.char_height() as f64;
        let step = if cell_h > 0.0 {
            cell_h
        } else {
            adj.step_increment()
        };
        let max_val = (adj.upper() - adj.page_size()).max(adj.lower());
        let value = (adj.value() + step * lines as f64).clamp(adj.lower(), max_val);
        adj.set_value(value);
    }

    /// Select all completed blocks as one range, with the newest block active.
    pub fn select_all_blocks(&self) {
        if self.fullscreen.get() {
            return;
        }
        self.cross_selection.clear_all();
        let finished = self.finished_blocks.borrow();
        let (Some(first), Some(last)) = (finished.first(), finished.last()) else {
            return;
        };
        {
            let mut selected = self.selected_block_ids.borrow_mut();
            selected.clear();
            selected.extend(finished.iter().map(|block| block.id));
        }
        self.selection_anchor_id.set(Some(first.id));
        self.selected_block_id.set(Some(last.id));
        sync_finished_block_selection(&finished, &self.selected_block_ids, &self.selected_block_id);
        self.active.borrow().grab_focus();
    }

    /// Reinsert all selected commands in terminal order without executing them.
    pub fn reinput_selected_commands(&self) {
        if self.fullscreen.get() {
            return;
        }
        let finished = self.finished_blocks.borrow();
        let recalled = {
            let selected = self.selected_block_ids.borrow();
            recall_selected_commands_at_prompt(
                &self.pty,
                &self.pty_synced,
                &self.typed_cmd,
                self.bstate.get(),
                &finished,
                &selected,
                self.bracketed_paste.get(),
            )
        };
        if recalled {
            clear_finished_block_selection(
                &finished,
                &self.selected_block_ids,
                &self.selected_block_id,
                &self.selection_anchor_id,
            );
            self.active.borrow().grab_focus();
        }
    }

    /// Remove every completed block and all block-indexed UI state.
    pub fn clear_blocks(&self) {
        // A background load that completes after Clear must not resurrect the
        // just-deleted history, and its shutdown merge must not prepend it to
        // the empty replacement snapshot.
        self.history_load.discard();
        self.clear_find();
        self.active_vte.unselect_all();

        let widgets: Vec<gtk4::Box> = self
            .finished_blocks
            .borrow_mut()
            .drain(..)
            .map(|block| block.widget().clone())
            .collect();
        let mut pool = self.widget_pool.borrow_mut();
        for widget in widgets {
            self.block_list.remove(&widget);
            pool.release(widget);
        }
        drop(pool);

        self.block_data.borrow_mut().clear();
        self.bookmarks.borrow_mut().clear();
        self.visible_indices.borrow_mut().clear();
        self.selected_block_ids.borrow_mut().clear();
        self.selected_block_id.set(None);
        self.selection_anchor_id.set(None);
        self.unread_count.set(0);
        set_jump_fab_label(&self.jump_fab, 0);
        self.jump_fab.set_visible(false);
        {
            let mut viewport = self.viewport.borrow_mut();
            viewport.first_visible = 0;
            viewport.last_visible = 0;
            viewport.total_height = 0;
        }
        self.block_list.queue_allocate();

        // Never inject form-feed into a running/full-screen process.
        if self.bstate.get() == BlockState::AwaitingCommand {
            if let Err(error) = self.pty.write_bytes(b"\x0c") {
                self.pty
                    .report_write_error("could not queue terminal clear", error);
            }
        }
        if let Err(err) = self.save_history() {
            log::warn!("save cleared block history: {err}");
        }
    }

    pub fn apply_failed_filter(&self) {
        if let Some(idx) = self.get_failed_blocks().first().copied() {
            self.scroll_to_block(idx);
        }
    }

    pub fn apply_slow_filter(&self) {
        if let Some(idx) = self.get_slow_blocks(1000).first().copied() {
            self.scroll_to_block(idx);
        }
    }

    pub fn apply_pinned_filter(&self) {
        let finished = self.finished_blocks.borrow();
        let bookmarks = self.bookmarks.borrow();
        if let Some((idx, _)) = finished
            .iter()
            .enumerate()
            .find(|(_, block)| bookmarks.contains(&block.id))
        {
            drop(bookmarks);
            drop(finished);
            self.scroll_to_block(idx);
        }
    }

    pub fn clear_block_filter(&self) {
        self.scroll_to_block(0);
    }

    pub fn jump_to_pinned(&self, direction: i32) {
        let finished = self.finished_blocks.borrow();
        let bookmarks = self.bookmarks.borrow();
        if bookmarks.is_empty() {
            return;
        }
        let marked: Vec<usize> = finished
            .iter()
            .enumerate()
            .filter(|(_, block)| bookmarks.contains(&block.id))
            .map(|(idx, _)| idx)
            .collect();
        if marked.is_empty() {
            return;
        }
        let cur = self
            .selected_block_id
            .get()
            .and_then(|id| finished.iter().position(|block| block.id == id));
        let target = if direction < 0 {
            marked
                .iter()
                .rev()
                .find(|&&idx| cur.map(|c| idx < c).unwrap_or(true))
                .copied()
                .or_else(|| marked.last().copied())
        } else {
            marked
                .iter()
                .find(|&&idx| cur.map(|c| idx > c).unwrap_or(true))
                .copied()
                .or_else(|| marked.first().copied())
        };
        drop(bookmarks);
        drop(finished);
        if let Some(idx) = target {
            self.scroll_to_block(idx);
        }
    }

    /// Apply updated theme colors to the block widgets and the live VTE.
    pub fn apply_theme(&self) {
        let config = self.config.borrow();
        apply_terminal_theme(&self.active_vte, &config);
        for block in self.finished_blocks.borrow().iter() {
            apply_snapshot_theme_to_vte(&block.command_vte, &config);
            apply_snapshot_theme_to_vte(&block.output_vte, &config);
        }
        install_block_css(&config);
    }

    /// Update font for VTE terminal and block view CSS.
    pub fn set_font(&self, font_desc: &FontDescription) {
        self.active_vte.set_font(Some(font_desc));
        for block in self.finished_blocks.borrow().iter() {
            block.command_vte.set_font(Some(font_desc));
            block.output_vte.set_font(Some(font_desc));
        }
        // Update config and regenerate CSS with new font
        self.config.borrow_mut().font_desc = font_desc.to_string();
        install_block_css(&self.config.borrow());
    }

    /// Update font scale for VTE terminal and block view CSS.
    pub fn set_font_scale(&self, scale: f64) {
        self.active_vte.set_font_scale(scale);
        for block in self.finished_blocks.borrow().iter() {
            block.command_vte.set_font_scale(scale);
            block.output_vte.set_font_scale(scale);
        }
        self.config.borrow_mut().default_font_scale = scale;
        // Regenerate CSS with updated font scale
        install_block_css(&self.config.borrow());
    }

    /// Update virtual scrolling viewport state based on scroll position.
    pub fn update_viewport(&self) {
        let adj = self.block_scroll.vadjustment();
        let block_data = self.block_data.borrow();
        let Some(next_viewport) = viewport_state_for_scroll(
            &block_data,
            adj.value(),
            adj.page_size(),
            self.config.borrow().virtual_scroll_margin,
        ) else {
            return;
        };
        drop(block_data);

        let mut vp = self.viewport.borrow_mut();
        *vp = next_viewport;
    }

    /// Update block visibility based on viewport: show visible blocks, hide off-screen ones.
    pub fn update_block_visibility(&self) {
        let vp = self.viewport.borrow().clone();
        let new_visible = visible_indices_for_viewport(&vp);

        let finished = self.finished_blocks.borrow();
        let mut block_data = self.block_data.borrow_mut();
        let mut visible = self.visible_indices.borrow_mut();
        apply_visible_indices(&finished, &mut block_data, &mut visible, new_visible);
    }

    /// Collect a snapshot of internal runtime state for the debug dashboard.
    /// Returns labelled sections, each a list of (key, value) rows.
    pub fn debug_info(&self) -> Vec<(&'static str, Vec<(String, String)>)> {
        let out_cols = self.active_vte.column_count();
        let out_rows = self.active_vte.row_count();

        let finished_len = self.finished_blocks.borrow().len();
        let block_data_len = self.block_data.borrow().len();
        let failed = self.get_failed_blocks().len();
        let slow = self.get_slow_blocks(1000).len();
        let total_output_bytes: usize = self
            .block_data
            .borrow()
            .iter()
            .map(|b| b.output.len())
            .sum();
        let viewport = self.viewport.borrow().clone();
        let visible = self.visible_indices.borrow().len();
        let selected = self
            .selected_block_id
            .get()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string());
        let selected_count = self.selected_block_ids.borrow().len();

        vec![
            (
                "State",
                vec![
                    (
                        "Block state".to_string(),
                        format!("{:?}", self.bstate.get()),
                    ),
                    (
                        "Mouse reporting".to_string(),
                        format!("{:?}", self.mouse_reporting_mode.get()),
                    ),
                    (
                        "Alt screen visible".to_string(),
                        self.fullscreen.get().to_string(),
                    ),
                ],
            ),
            (
                "PTY",
                vec![
                    ("PID".to_string(), self.pty.pid_i32().to_string()),
                    ("CWD".to_string(), self.current_cwd.borrow().clone()),
                    (
                        "Output grid".to_string(),
                        format!("{out_cols} × {out_rows}"),
                    ),
                ],
            ),
            (
                "Blocks",
                vec![
                    ("Finished blocks".to_string(), finished_len.to_string()),
                    ("Block data entries".to_string(), block_data_len.to_string()),
                    ("Failed blocks".to_string(), failed.to_string()),
                    ("Slow blocks (>1s)".to_string(), slow.to_string()),
                    (
                        "Total output bytes".to_string(),
                        total_output_bytes.to_string(),
                    ),
                    ("Selected blocks".to_string(), selected_count.to_string()),
                    ("Selected block id".to_string(), selected),
                ],
            ),
            (
                "Viewport",
                vec![
                    (
                        "First visible".to_string(),
                        viewport.first_visible.to_string(),
                    ),
                    (
                        "Last visible".to_string(),
                        viewport.last_visible.to_string(),
                    ),
                    (
                        "Total height".to_string(),
                        format!("{}px", viewport.total_height),
                    ),
                    ("Realized widgets".to_string(), visible.to_string()),
                    ("Profiling".to_string(), prof_enabled().to_string()),
                ],
            ),
        ]
    }

    pub fn scroll_to_block(&self, block_index: usize) {
        let finished = self.finished_blocks.borrow();
        if let Some(block) = finished.get(block_index) {
            self.cross_selection.clear_all();
            replace_finished_block_selection(
                &finished,
                &self.selected_block_ids,
                &self.selected_block_id,
                &self.selection_anchor_id,
                Some(block.id),
            );
            scroll_finished_block_into_view(&self.block_scroll, block);
        }
    }

    /// Delete a block by ID while keeping every parallel block-mode state in sync.
    pub fn delete_block_by_id(&self, block_id: u64) {
        let _ = remove_finished_block(
            block_id,
            &self.finished_blocks,
            &self.block_data,
            &self.block_list,
            BlockSelectionRefs {
                ids: &self.selected_block_ids,
                active: &self.selected_block_id,
                anchor: &self.selection_anchor_id,
            },
            &self.bookmarks,
            &self.visible_indices,
        );
    }

    /// Most-recent-first deduplicated list of finished-block command lines.
    /// Used to populate the Ctrl+Shift+H history palette. The first entry is
    /// the most recent unique command; whitespace-only commands are dropped.
    pub fn command_history(&self) -> Vec<String> {
        let finished = self.finished_blocks.borrow();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for block in finished.iter().rev() {
            let cmd = block.cmd_text.trim();
            if cmd.is_empty() {
                continue;
            }
            if seen.insert(cmd.to_string()) {
                out.push(cmd.to_string());
            }
        }
        out
    }

    /// Snapshot the currently selected finished block as an `ai::BlockContext`,
    /// truncating the output to `head + tail = 2*lines_per_side + 1` lines so
    /// a `cargo build` block doesn't blow the request budget. Returns `None`
    /// when no block is selected (Ctrl+Shift+Q from the live cell etc.).
    pub fn selected_block_context(&self, lines_per_side: usize) -> Option<crate::ai::BlockContext> {
        let id = self.selected_block_id.get()?;
        let finished = self.finished_blocks.borrow();
        let block = finished.iter().find(|b| b.id == id)?;
        let data = self.block_data.borrow();
        let bd = data.iter().find(|b| b.id == id);

        let (output, truncated) = block.with_stripped_output(|raw| {
            let output = crate::ai::truncate_for_context(raw, lines_per_side);
            let truncated = output != raw;
            (output, truncated)
        });
        // A block with no BlockData row (history not loaded) is not the same as
        // one whose shell reported no status, but neither is a success: both go
        // to the model as the sentinel plus the note it can actually read.
        let (exit_code, unknown_note) = exit_code_for_shared_surface(bd.and_then(|b| b.exit_code));
        let output = match unknown_note {
            Some(note) => format!("{note}\n{output}"),
            None => output,
        };
        Some(crate::ai::BlockContext {
            cmd: crate::review_input::safe_multiline_display(
                &block.cmd_text,
                MAX_COMMAND_CAPTURE_BYTES,
            ),
            output: crate::review_input::safe_multiline_display(&output, 128 * 1024),
            cwd: bd
                .and_then(|b| b.cwd.as_deref())
                .map(|cwd| crate::review_input::safe_inline_display(cwd, 16 * 1024)),
            exit_code,
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        append_bounded_text_tail, apply_vte_commit_to_shadow, approved_command_submission_payload,
        background_output_has_visible_text, bounded_journal_output, build_clipboard_paste,
        build_command_recall, build_keyboard_query_reply, classify_command_prompt_status,
        coalesce_bytes_events, collapse_repaint_output, command_capture_range_is_bounded,
        compute_viewport_state, format_color_query_reply, history_edge_navigation_available,
        normalize_captured_command, normalize_loaded_block_ids, notification_allowed,
        output_has_vertical_repaint, parse_color_spec, record_external_input,
        resolve_command_for_block, resolve_submitted_command, scroll_delta_to_reveal,
        selected_command_text, selected_id_range, should_buffer_background_output,
        stable_visible_indices, strip_ansi, strip_ansi_with_clear_detect, take_background_output,
        truncate_plain_output_for_height, viewport_page_size_changed, viewport_state_for_scroll,
        visible_indices_for_viewport, vte_commit_shadow_rollback, BlockData, BlockState,
        BoundedByteRing, CommandMeta, CommandPromptStatus, DynamicColors, PendingCommandMeta,
        ViewportState, MAX_COMMAND_CAPTURE_BYTES, MAX_JOURNAL_OUTPUT_BYTES,
        MAX_PROMPT_CAPTURE_BYTES, MAX_RAW_OUTPUT_BYTES, TRUNCATED_COMMAND_PLACEHOLDER,
    };
    use crate::parser::{ColorKind, KeyboardProtocolQuery, ParserEvent};
    use gtk4::gdk::RGBA;
    use std::cell::{Cell, RefCell};
    use std::collections::{HashSet, VecDeque};
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn constructor_propagates_injected_spawn_failure_without_panicking() {
        let config = crate::config::load_safe_config().0;
        let shell_argv = vec!["injected-shell".to_string()];
        let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::TermView::new_with_spawner(
                &config,
                &shell_argv,
                None,
                Some("injected-session"),
                &[],
                |_argv, _cwd, _env| {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "injected PTY spawn failure",
                    ))
                },
            )
        }));

        let construction = match attempt {
            Ok(construction) => construction,
            Err(_) => panic!("TermView construction panicked on a PTY spawn error"),
        };
        let error = match construction {
            Ok(_) => panic!("injected PTY spawn failure unexpectedly constructed a TermView"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert_eq!(error.to_string(), "injected PTY spawn failure");
    }

    // ── Dynamic OSC 10/11/12 color tracking ──────────────────────────────

    /// Resolve one query the way the reader callback does: dynamic override
    /// when present, theme color otherwise.
    fn color_reply(dynamic: &DynamicColors, kind: ColorKind, theme: RGBA) -> String {
        format_color_query_reply(kind, dynamic.get(kind).unwrap_or(theme))
    }

    #[test]
    fn dynamic_color_set_changes_query_reply() {
        let theme_bg = RGBA::parse("#121616").unwrap();
        let mut dynamic = DynamicColors::default();

        // Untouched tracker: the query reports the theme color.
        assert_eq!(
            color_reply(&dynamic, ColorKind::Background, theme_bg),
            "\x1b]11;rgb:1212/1616/1616\x1b\\"
        );

        // OSC 11 set (X11 rgb: form): the next query reports the dynamic color.
        dynamic.set(ColorKind::Background, "rgb:1e1e/2e2e/3e3e");
        assert_eq!(
            color_reply(&dynamic, ColorKind::Background, theme_bg),
            "\x1b]11;rgb:1e1e/2e2e/3e3e\x1b\\"
        );

        // Hex form updates the same slot; other slots stay on the theme.
        dynamic.set(ColorKind::Background, "#ff8800");
        assert_eq!(
            color_reply(&dynamic, ColorKind::Background, theme_bg),
            "\x1b]11;rgb:ffff/8888/0000\x1b\\"
        );
        assert_eq!(dynamic.get(ColorKind::Foreground), None);
        assert_eq!(dynamic.get(ColorKind::Cursor), None);
    }

    #[test]
    fn dynamic_color_reset_restores_theme_reply() {
        let theme_fg = RGBA::parse("#f8f7e9").unwrap();
        let mut dynamic = DynamicColors::default();

        dynamic.set(ColorKind::Foreground, "rgb:ffff/0000/0000");
        assert_eq!(
            color_reply(&dynamic, ColorKind::Foreground, theme_fg),
            "\x1b]10;rgb:ffff/0000/0000\x1b\\"
        );

        // OSC 110: the tracked value drops, queries answer the theme again.
        dynamic.reset(ColorKind::Foreground);
        assert_eq!(dynamic.get(ColorKind::Foreground), None);
        assert_eq!(
            color_reply(&dynamic, ColorKind::Foreground, theme_fg),
            "\x1b]10;rgb:f8f8/f7f7/e9e9\x1b\\"
        );
    }

    #[test]
    fn dynamic_color_junk_spec_is_ignored() {
        let mut dynamic = DynamicColors::default();
        dynamic.set(ColorKind::Background, "definitely-not-a-color!!");
        assert_eq!(dynamic, DynamicColors::default());

        // A junk set must not clobber a previously tracked value either.
        dynamic.set(ColorKind::Background, "rgb:0000/8888/ffff");
        let tracked = dynamic;
        dynamic.set(ColorKind::Background, "rgb:zz/zz/zz");
        dynamic.set(ColorKind::Background, "rgb:1/2");
        dynamic.set(ColorKind::Background, "rgb:1/2/3/4");
        dynamic.set(ColorKind::Background, "rgb:12345/1/1");
        dynamic.set(ColorKind::Background, "");
        assert_eq!(dynamic, tracked);

        // Palette sets are VTE-native and never tracked.
        dynamic.set(ColorKind::Palette(3), "#ffffff");
        assert_eq!(dynamic, tracked);
        assert_eq!(dynamic.get(ColorKind::Palette(3)), None);
    }

    #[test]
    fn color_spec_parses_x11_rgb_scaling_and_names() {
        // 1/2/4-digit channels each scale against their own width: `f`, `ff`,
        // and `ffff` all mean the full-intensity channel (XParseColor rules).
        let full = parse_color_spec("rgb:f/ff/ffff").unwrap();
        assert_eq!((full.red(), full.green(), full.blue()), (1.0, 1.0, 1.0));
        // Named and hex forms fall through to gdk's parser.
        assert_eq!(parse_color_spec("red"), RGBA::parse("#ff0000").ok());
        assert!(parse_color_spec(" #0af ").is_some());
        assert_eq!(parse_color_spec("nonsense"), None);
    }

    #[test]
    fn notification_rate_limit_spacing() {
        use std::time::{Duration, Instant};
        let start = Instant::now();
        // First notification ever is always allowed.
        assert!(notification_allowed(None, start));
        // A burst right after the first one is dropped...
        assert!(!notification_allowed(Some(start), start));
        assert!(!notification_allowed(
            Some(start),
            start + Duration::from_millis(1999)
        ));
        // ...until the two-second window has fully elapsed.
        assert!(notification_allowed(
            Some(start),
            start + Duration::from_secs(2)
        ));
        assert!(notification_allowed(
            Some(start),
            start + Duration::from_secs(5)
        ));
    }

    #[test]
    fn background_output_requires_visible_text() {
        assert!(!background_output_has_visible_text(b"\r\n\x1b[0m"));
        assert!(background_output_has_visible_text(
            b"\x1b[36mworker finished\x1b[0m\r\n"
        ));
    }

    #[test]
    fn taking_background_output_drains_the_pending_buffer() {
        let mut bytes = BoundedByteRing::new(MAX_RAW_OUTPUT_BYTES);
        bytes.append(b"async line\r\n");
        let pending = RefCell::new(bytes);
        assert_eq!(
            take_background_output(&pending).as_deref(),
            Some("async line\r\n")
        );
        assert!(pending.borrow().is_empty());
        assert!(take_background_output(&pending).is_none());
    }

    #[test]
    fn programmatic_editor_sync_keeps_async_output_inline() {
        assert!(should_buffer_background_output(false, false));
        assert!(!should_buffer_background_output(true, false));
        assert!(!should_buffer_background_output(false, true));
    }

    #[test]
    fn command_prompt_status_explains_each_agent_gate() {
        assert_eq!(
            classify_command_prompt_status(BlockState::AwaitingCommand, false, false, false, true),
            CommandPromptStatus::Ready
        );
        for (dirty, synced, typed_empty) in [
            (true, false, true),
            (false, true, true),
            (false, false, false),
        ] {
            assert_eq!(
                classify_command_prompt_status(
                    BlockState::AwaitingCommand,
                    false,
                    dirty,
                    synced,
                    typed_empty
                ),
                CommandPromptStatus::HasInput
            );
        }
        assert_eq!(
            classify_command_prompt_status(BlockState::CollectingOutput, false, false, false, true),
            CommandPromptStatus::Running
        );
        assert_eq!(
            classify_command_prompt_status(BlockState::RawFallback, false, false, false, true),
            CommandPromptStatus::ShellIntegrationUnavailable
        );
        assert_eq!(
            classify_command_prompt_status(BlockState::AwaitingCommand, true, false, false, true),
            CommandPromptStatus::Fullscreen
        );
    }

    #[test]
    fn restored_block_ids_are_unique_and_reserve_the_allocator() {
        let mut blocks = VecDeque::from([
            block_with_height(10),
            block_with_height(20),
            block_with_height(30),
        ]);
        blocks[0].id = 4;
        blocks[1].id = 4;
        blocks[2].id = 9;
        let counter = AtomicU64::new(0);

        assert_eq!(normalize_loaded_block_ids(&mut blocks, &counter), 1);
        let ids: HashSet<u64> = blocks.iter().map(|block| block.id).collect();
        assert_eq!(ids.len(), blocks.len());
        assert!(counter.load(Ordering::Relaxed) >= 11);
        assert_eq!(blocks[0].id, 4);
        assert_eq!(blocks[2].id, 9);
    }

    #[test]
    fn external_input_marks_the_editor_dirty_without_copying_escape_sequences() {
        let typed = RefCell::new(String::new());
        let synced = Cell::new(false);
        let dirty = Cell::new(false);

        assert!(record_external_input(
            BlockState::AwaitingCommand,
            b"hello\nworld",
            &typed,
            &synced,
            &dirty,
        ));
        assert_eq!(&*typed.borrow(), "hello\nworld");
        assert!(synced.get());
        assert!(dirty.get());

        synced.set(false);
        dirty.set(false);
        assert!(record_external_input(
            BlockState::AwaitingCommand,
            b"\x1b[D",
            &typed,
            &synced,
            &dirty,
        ));
        assert_eq!(&*typed.borrow(), "hello\nworld");
        assert!(synced.get());
        assert!(dirty.get());

        assert!(!record_external_input(
            BlockState::CollectingOutput,
            b"ignored",
            &typed,
            &synced,
            &dirty,
        ));
    }

    #[test]
    fn command_shadow_fails_closed_after_overflow_and_cannot_be_backspaced_empty() {
        let typed = RefCell::new("x".repeat(MAX_COMMAND_CAPTURE_BYTES));
        let synced = Cell::new(false);
        let dirty = Cell::new(false);
        assert!(record_external_input(
            BlockState::AwaitingCommand,
            b"y",
            &typed,
            &synced,
            &dirty,
        ));
        assert_eq!(&*typed.borrow(), TRUNCATED_COMMAND_PLACEHOLDER);
        assert!(record_external_input(
            BlockState::AwaitingCommand,
            b"\x7f",
            &typed,
            &synced,
            &dirty,
        ));
        assert_eq!(&*typed.borrow(), TRUNCATED_COMMAND_PLACEHOLDER);
        assert!(record_external_input(
            BlockState::AwaitingCommand,
            b"\x15",
            &typed,
            &synced,
            &dirty,
        ));
        assert!(typed.borrow().is_empty());
    }

    #[test]
    fn prompt_capture_retains_a_bounded_utf8_tail() {
        let mut prompt = "old".repeat(MAX_PROMPT_CAPTURE_BYTES);
        append_bounded_text_tail(&mut prompt, "界-new-prompt", MAX_PROMPT_CAPTURE_BYTES);
        assert!(prompt.len() <= MAX_PROMPT_CAPTURE_BYTES);
        assert!(prompt.ends_with("界-new-prompt"));
        assert!(std::str::from_utf8(prompt.as_bytes()).is_ok());
    }

    #[test]
    fn command_capture_range_is_rejected_before_a_large_vte_allocation() {
        assert!(command_capture_range_is_bounded(10, 10, 120));
        assert!(!command_capture_range_is_bounded(10, 9, 120));
        assert!(!command_capture_range_is_bounded(
            0,
            MAX_COMMAND_CAPTURE_BYTES as i64,
            120
        ));
    }

    #[test]
    fn selected_command_aggregation_is_atomic_at_the_review_limit() {
        let selected = HashSet::from([1, 2]);
        let too_large = "x".repeat(MAX_COMMAND_CAPTURE_BYTES);
        assert!(selected_command_text([(1, too_large.as_str()), (2, "y")], &selected).is_empty());
        assert_eq!(
            selected_command_text([(1, "one"), (2, "two")], &selected),
            "one\ntwo"
        );
    }

    #[test]
    fn clipboard_paste_matches_the_effective_editor_text() {
        let unbracketed = build_clipboard_paste("one\r\ntwo", false);
        assert_eq!(unbracketed.echo_text, "one");
        assert_eq!(unbracketed.bytes, b"one".to_vec());

        let bracketed = build_clipboard_paste("one\r\ntwo", true);
        assert_eq!(bracketed.echo_text, "one\ntwo");
        assert_eq!(bracketed.bytes, b"\x1b[200~one\ntwo\x1b[201~".to_vec());
    }

    /// The clipboard is the hostile input this round's shared encoder exists
    /// for: an embedded terminator must be removed from the body — and surfaced
    /// on the risk report — instead of closing the frame early.
    #[test]
    fn clipboard_paste_defuses_an_embedded_frame_terminator() {
        let paste = build_clipboard_paste("docs\x1b[201~\rrm -rf ~\r", true);
        assert!(paste.risk.had_embedded_paste_marker);
        assert!(paste.bytes.starts_with(b"\x1b[200~"));
        assert!(paste.bytes.ends_with(b"\x1b[201~"));
        let interior = &paste.bytes[6..paste.bytes.len() - 6];
        assert!(
            !interior.windows(6).any(|window| window == b"\x1b[201~"),
            "terminator survived in the body: {:?}",
            String::from_utf8_lossy(&paste.bytes)
        );
    }

    #[test]
    fn home_end_return_to_readline_after_the_editor_is_dirty() {
        assert!(history_edge_navigation_available(
            BlockState::AwaitingCommand,
            false
        ));
        assert!(!history_edge_navigation_available(
            BlockState::AwaitingCommand,
            true
        ));
        assert!(!history_edge_navigation_available(
            BlockState::CollectingOutput,
            false
        ));
    }

    fn ev_summary(events: &[ParserEvent]) -> Vec<String> {
        events
            .iter()
            .map(|e| match e {
                ParserEvent::Bytes(b) => format!("B({})", String::from_utf8_lossy(b)),
                ParserEvent::PromptStart => "PS".to_string(),
                ParserEvent::PromptEnd => "PE".to_string(),
                ParserEvent::CommandStart(_) => "CS".to_string(),
                ParserEvent::CommandEnd { exit, .. } => match exit {
                    Some(code) => format!("CE({code})"),
                    None => "CE(?)".to_string(),
                },
                ParserEvent::AltScreenEnter(mode) => format!("ALT+({mode})"),
                ParserEvent::AltScreenLeave(mode) => format!("ALT-({mode})"),
                _ => "?".to_string(),
            })
            .collect()
    }

    #[test]
    fn captured_command_drops_early_prompt_marker_prefix() {
        assert_eq!(normalize_captured_command("yj ~ ❯ pwd", "yj ~ ❯"), "pwd");
    }

    /// jsh puts the command it parsed on the OSC 133 ;C packet. That beats the
    /// VTE screen scrape, which can pick up a redraw or an accepted
    /// autosuggestion instead of what actually ran.
    #[test]
    fn shell_metadata_outranks_the_screen_reconstruction() {
        let meta = CommandMeta {
            command: Some("git status --short".to_string()),
            ..CommandMeta::default()
        };
        assert_eq!(
            resolve_command_for_block(&meta, "git stat"),
            "git status --short"
        );
    }

    /// A shell that sends the bare mark keeps the old reconstruction path.
    #[test]
    fn bare_mark_falls_back_to_the_reconstruction() {
        let meta = CommandMeta::default();
        assert_eq!(resolve_command_for_block(&meta, "  ls -la "), "ls -la");
        assert_eq!(resolve_command_for_block(&meta, "   "), "");
    }

    /// `cmd_truncated=1` is its own case: the shell *had* a command line and said
    /// so, which is worth distinguishing from a shell that sends no metadata at
    /// all. The reconstruction still wins when it captured something.
    #[test]
    fn a_truncated_command_line_is_labelled_not_guessed() {
        let meta = CommandMeta {
            command: None,
            command_truncated: true,
            ..CommandMeta::default()
        };
        assert_eq!(
            resolve_command_for_block(&meta, ""),
            TRUNCATED_COMMAND_PLACEHOLDER
        );
        assert_eq!(resolve_command_for_block(&meta, "ls"), "ls");
    }

    /// jsh attaches the duration to `D` and the id to `C`; folding the two marks
    /// must not let the second one erase what the first carried.
    #[test]
    fn command_metadata_merges_across_both_marks() {
        let mut pending = PendingCommandMeta::from_command_start(&CommandMeta {
            id: Some("jsh-7".to_string()),
            cwd: Some("/tmp/project".to_string()),
            ..CommandMeta::default()
        });
        pending.merge_command_end(&CommandMeta {
            duration_ms: Some(1234),
            // jsh's D packet reports the cwd *after* the command; a `cd` must not
            // relabel the block with the directory it moved to.
            cwd: Some("/tmp/elsewhere".to_string()),
            ..CommandMeta::default()
        });
        assert_eq!(pending.id.as_deref(), Some("jsh-7"));
        assert_eq!(pending.cwd.as_deref(), Some("/tmp/project"));
        assert_eq!(pending.duration_ms, Some(1234));

        // A shell that only labels its D mark still gets a cwd.
        let mut only_end = PendingCommandMeta::from_command_start(&CommandMeta::default());
        only_end.merge_command_end(&CommandMeta {
            cwd: Some("/srv".to_string()),
            ..CommandMeta::default()
        });
        assert_eq!(only_end.cwd.as_deref(), Some("/srv"));

        let rejected = PendingCommandMeta::from_command_start(&CommandMeta {
            id: Some("bad/id".to_string()),
            cwd: Some("/tmp/safe\u{202e}fake".to_string()),
            ..CommandMeta::default()
        });
        assert_eq!(rejected.id, None);
        assert_eq!(rejected.cwd, None);
    }

    #[test]
    fn unsafe_shell_command_metadata_falls_back_to_the_visible_capture() {
        let hidden = CommandMeta {
            command: Some("echo safe\u{202e}fake".to_string()),
            ..CommandMeta::default()
        };
        assert_eq!(
            resolve_command_for_block(&hidden, "echo visible"),
            "echo visible"
        );
        let oversized = CommandMeta {
            command: Some("x".repeat(MAX_COMMAND_CAPTURE_BYTES + 1)),
            ..CommandMeta::default()
        };
        assert_eq!(resolve_command_for_block(&oversized, ""), "");
    }

    #[test]
    fn journal_output_keeps_the_tail_on_a_char_boundary() {
        let short = "error: nope";
        assert_eq!(
            bounded_journal_output(short),
            (short.to_string(), false),
            "output within the bound is submitted whole"
        );

        // Multi-byte scalars straddling the cut must not be split: the journal
        // is JSON, and half a scalar does not encode.
        let long = "。".repeat(MAX_JOURNAL_OUTPUT_BYTES);
        let (text, truncated) = bounded_journal_output(&long);
        assert!(truncated);
        assert!(text.len() <= MAX_JOURNAL_OUTPUT_BYTES);
        assert!(long.ends_with(&text), "the tail is what survives");
        assert!(text.chars().all(|ch| ch == '。'));
    }

    #[test]
    fn captured_command_preserves_legitimate_text() {
        assert_eq!(
            normalize_captured_command("printf pwd", "yj ~ ❯"),
            "printf pwd"
        );
    }

    #[test]
    fn submitted_command_falls_back_when_vte_echo_has_not_settled() {
        assert_eq!(
            resolve_submitted_command("", "yj ~/project ❯", "git status", None),
            "git status"
        );
    }

    #[test]
    fn submitted_command_prefers_the_rendered_line_editor_state() {
        assert_eq!(
            resolve_submitted_command("git diff --stat", "yj ~/project ❯", "git status", None),
            "git diff --stat"
        );
        assert_eq!(
            resolve_submitted_command("yj ~/project ❯ cargo test", "yj ~/project ❯", "cargo", None),
            "cargo test"
        );
    }

    #[test]
    fn programmatic_submission_wins_over_stale_vte_capture() {
        assert_eq!(
            resolve_submitted_command(
                "ls",
                "yj ~/project ❯",
                "cat monitor_xilem_bar.sh",
                Some("cat monitor_xilem_bar.sh")
            ),
            "cat monitor_xilem_bar.sh"
        );
    }

    #[test]
    fn keyboard_protocol_queries_have_safe_fallback_replies() {
        assert_eq!(
            build_keyboard_query_reply(KeyboardProtocolQuery::KittyQuery, 0, 0),
            "\x1b[?0u"
        );
        assert_eq!(
            build_keyboard_query_reply(KeyboardProtocolQuery::ModifyOtherKeysQuery, 0, 0),
            "\x1b[>4;0m"
        );
        assert_eq!(
            build_keyboard_query_reply(KeyboardProtocolQuery::PrimaryDeviceAttributes, 0, 0),
            "\x1b[?1;2c"
        );
        assert_eq!(
            build_keyboard_query_reply(KeyboardProtocolQuery::DeviceStatus, 0, 0),
            "\x1b[0n"
        );
        assert_eq!(
            build_keyboard_query_reply(KeyboardProtocolQuery::CursorPosition, 4, 2),
            "\x1b[3;5R"
        );
        assert_eq!(
            build_keyboard_query_reply(KeyboardProtocolQuery::CursorPosition, -8, -2),
            "\x1b[1;1R"
        );

        let version = build_keyboard_query_reply(KeyboardProtocolQuery::XtVersion, 0, 0);
        assert!(version.contains(env!("CARGO_PKG_VERSION")));
        assert!(version.starts_with("\x1bP>|jterm4 "));
        assert!(version.ends_with("\x1b\\"));
    }

    fn block_with_height(estimated_height: i32) -> BlockData {
        BlockData {
            id: 0,
            prompt: String::new(),
            cmd: String::new(),
            cmd_markup: None,
            output: String::new(),
            exit_code: Some(0),
            estimated_height,
            line_count: 0,
            start_time_ms: None,
            end_time_ms: None,
            duration_ms: None,
            cwd: None,
            cols: 0,
        }
    }

    #[test]
    fn viewport_state_keeps_total_height_after_visible_range() {
        let blocks: VecDeque<BlockData> = [10, 20, 30, 40]
            .into_iter()
            .map(block_with_height)
            .collect();

        let vp = compute_viewport_state(&blocks, 15, 55);

        assert_eq!(vp.first_visible, 1);
        assert_eq!(vp.last_visible, 2);
        assert_eq!(vp.total_height, 100);
    }

    #[test]
    fn zero_sized_tab_viewport_is_ignored() {
        let blocks: VecDeque<BlockData> = [10, 20, 30, 40]
            .into_iter()
            .map(block_with_height)
            .collect();

        // Hidden GtkNotebook pages transiently expose page_size == 0. At this
        // exact block boundary the raw range would have first=2, last=1 and
        // virtualize every card.
        assert!(viewport_state_for_scroll(&blocks, 30.0, 0.0, 0).is_none());
        assert!(viewport_state_for_scroll(&blocks, 30.0, 0.5, 0).is_none());
    }

    #[test]
    fn upper_only_adjustment_changes_do_not_recompute_visibility() {
        let last_page_size = Cell::new(None);

        assert!(viewport_page_size_changed(&last_page_size, 300.0));
        // Adjustment::changed also fires when virtualization changes only
        // `upper`; the unchanged viewport extent must not feed that mutation
        // back into another visibility pass.
        assert!(!viewport_page_size_changed(&last_page_size, 300.0));
        assert!(!viewport_page_size_changed(&last_page_size, 300.4));
        assert!(viewport_page_size_changed(&last_page_size, 301.0));
        assert!(!viewport_page_size_changed(&last_page_size, f64::NAN));
    }

    #[test]
    fn remapped_tab_viewport_restores_visible_range() {
        let blocks: VecDeque<BlockData> = [10, 20, 30, 40]
            .into_iter()
            .map(block_with_height)
            .collect();

        let vp = viewport_state_for_scroll(&blocks, 30.0, 40.0, 0)
            .expect("mapped viewport should be valid");

        assert_eq!(vp.first_visible, 2);
        assert_eq!(vp.last_visible, 3);
        assert_eq!(visible_indices_for_viewport(&vp), HashSet::from([2, 3]));
    }

    #[test]
    fn boundary_jitter_cannot_toggle_a_rendered_block() {
        let blocks: VecDeque<BlockData> =
            std::iter::repeat_n(20, 10).map(block_with_height).collect();

        // Pinned near the bottom with one margin page: blocks 4..=9 are strict.
        let strict = viewport_state_for_scroll(&blocks, 120.0, 40.0, 1)
            .expect("strict viewport should be valid");
        let loose = viewport_state_for_scroll(&blocks, 120.0, 40.0, 2)
            .expect("loose viewport should be valid");
        assert_eq!(
            visible_indices_for_viewport(&strict),
            HashSet::from_iter(4..=9)
        );

        // Block 2 left the strict window after a sub-page clamp but is still
        // inside the loose one: it must stay rendered instead of oscillating.
        let current = HashSet::from_iter(2..=9);
        let next = stable_visible_indices(&strict, Some(&loose), &current);
        assert!(next.contains(&2));
        assert_eq!(next, HashSet::from_iter(2..=9));

        // A block outside even the loose window is released.
        let far_away = HashSet::from([0]);
        let next = stable_visible_indices(&strict, Some(&loose), &far_away);
        assert!(!next.contains(&0));
        assert_eq!(next, HashSet::from_iter(4..=9));

        // Hysteresis only preserves rendered state; it never devirtualizes a
        // band block that was already hidden.
        let none_currently_visible = HashSet::new();
        let next = stable_visible_indices(&strict, Some(&loose), &none_currently_visible);
        assert_eq!(next, HashSet::from_iter(4..=9));
    }

    #[test]
    fn visible_indices_are_capped_to_reasonable_window() {
        let vp = ViewportState {
            first_visible: 10,
            last_visible: 2_000,
            total_height: 0,
        };

        let visible = visible_indices_for_viewport(&vp);

        assert!(visible.contains(&10));
        assert!(visible.contains(&1010));
        assert!(!visible.contains(&1011));
        assert_eq!(visible.len(), 1001);
    }

    #[test]
    fn reveal_scroll_keeps_fully_visible_blocks_stable() {
        assert_eq!(scroll_delta_to_reveal(30.0, 80.0, 200.0, 18.0), 0.0);
    }

    #[test]
    fn reveal_scroll_moves_only_enough_for_clipped_blocks() {
        assert_eq!(scroll_delta_to_reveal(-12.0, 40.0, 200.0, 18.0), -30.0);
        assert_eq!(scroll_delta_to_reveal(180.0, 230.0, 200.0, 18.0), 48.0);
    }

    #[test]
    fn reveal_scroll_aligns_tall_blocks_to_the_top() {
        assert_eq!(scroll_delta_to_reveal(40.0, 260.0, 200.0, 18.0), 22.0);
    }

    #[test]
    fn selected_block_range_is_inclusive_in_both_directions() {
        let ids = [10, 20, 30, 40];
        assert_eq!(selected_id_range(&ids, 20, 40), [20, 30, 40]);
        assert_eq!(selected_id_range(&ids, 40, 20), [20, 30, 40]);
        assert_eq!(selected_id_range(&ids, 99, 30), [30]);
    }

    #[test]
    fn truncate_plain_output_passthrough_counts_trimmed_lines() {
        let (text, lines) = truncate_plain_output_for_height("\nalpha\nbeta\n", 10);

        assert_eq!(text, "alpha\nbeta");
        assert_eq!(lines, 2);
    }

    #[test]
    fn truncate_plain_output_collects_only_visible_prefix() {
        let (text, lines) = truncate_plain_output_for_height("a\nb\nc\nd", 2);

        assert_eq!(
            text,
            "a\nb\n\n[... truncated: 4 lines total, showing first 2]"
        );
        assert_eq!(lines, 4);
    }

    #[test]
    fn coalesce_merges_consecutive_bytes() {
        let mut events = vec![
            ParserEvent::Bytes(b"hello ".to_vec()),
            ParserEvent::Bytes(b"world".to_vec()),
            ParserEvent::Bytes(b"!".to_vec()),
        ];
        coalesce_bytes_events(&mut events);
        assert_eq!(ev_summary(&events), vec!["B(hello world!)"]);
    }

    #[test]
    fn coalesce_preserves_boundary_events_in_order() {
        let mut events = vec![
            ParserEvent::Bytes(b"$ ".to_vec()),
            ParserEvent::PromptEnd,
            ParserEvent::Bytes(b"ls".to_vec()),
            ParserEvent::Bytes(b" -la".to_vec()),
            ParserEvent::CommandStart(CommandMeta::default()),
            ParserEvent::Bytes(b"file1\n".to_vec()),
            ParserEvent::Bytes(b"file2\n".to_vec()),
            ParserEvent::CommandEnd {
                exit: Some(0),
                meta: CommandMeta::default(),
            },
            ParserEvent::PromptStart,
        ];
        coalesce_bytes_events(&mut events);
        assert_eq!(
            ev_summary(&events),
            vec![
                "B($ )",
                "PE",
                "B(ls -la)",
                "CS",
                "B(file1\nfile2\n)",
                "CE(0)",
                "PS",
            ]
        );
    }

    #[test]
    fn coalesce_noop_on_empty_or_single() {
        let mut empty: Vec<ParserEvent> = Vec::new();
        coalesce_bytes_events(&mut empty);
        assert!(empty.is_empty());

        let mut one = vec![ParserEvent::Bytes(b"x".to_vec())];
        coalesce_bytes_events(&mut one);
        assert_eq!(ev_summary(&one), vec!["B(x)"]);

        let mut one_boundary = vec![ParserEvent::PromptStart];
        coalesce_bytes_events(&mut one_boundary);
        assert_eq!(ev_summary(&one_boundary), vec!["PS"]);
    }

    #[test]
    fn coalesce_handles_only_boundary_events() {
        let mut events = vec![
            ParserEvent::PromptStart,
            ParserEvent::PromptEnd,
            ParserEvent::CommandStart(CommandMeta::default()),
            ParserEvent::CommandEnd {
                exit: Some(1),
                meta: CommandMeta::default(),
            },
        ];
        coalesce_bytes_events(&mut events);
        assert_eq!(ev_summary(&events), vec!["PS", "PE", "CS", "CE(1)"]);
    }

    #[test]
    fn strips_charset_designation_from_output() {
        assert_eq!(strip_ansi("\u{1b}(Btop"), "top");
    }

    #[test]
    fn cursor_home_and_partial_erase_do_not_clear_block_output() {
        assert_eq!(
            strip_ansi_with_clear_detect("\u{1b}[Hgit output"),
            ("git output".to_string(), false)
        );
        assert_eq!(
            strip_ansi_with_clear_detect("\u{1b}[Jgit output"),
            ("git output".to_string(), false)
        );
        assert_eq!(
            strip_ansi_with_clear_detect("\u{1b}[2Jfresh"),
            ("fresh".to_string(), true)
        );
    }

    // ── strip_ansi_with_clear_detect: cursor model tests ────────────────

    #[test]
    fn carriage_return_overwrites_line() {
        // \r moves cursor to col 0, shorter text overwrites prefix but leaves tail
        assert_eq!(
            strip_ansi_with_clear_detect("Loading...\rDone!"),
            ("Done!ng...".to_string(), false)
        );
    }

    #[test]
    fn carriage_return_full_overwrite() {
        // Full overwrite of same-length text
        assert_eq!(
            strip_ansi_with_clear_detect("AAAA\rBBBB"),
            ("BBBB".to_string(), false)
        );
    }

    #[test]
    fn spinner_animation_shows_final_frame() {
        // Simulates spinner: multiple frames separated by \r
        assert_eq!(
            strip_ansi_with_clear_detect("| working\r/ working\r- working\r\\ working"),
            ("\\ working".to_string(), false)
        );
    }

    #[test]
    fn csi_erase_line_to_end() {
        // CSI 0K: erase from cursor to end of line
        assert_eq!(
            strip_ansi_with_clear_detect("hello world\r\u{1b}[0Kdone"),
            ("done".to_string(), false)
        );
    }

    #[test]
    fn csi_erase_line_implicit_zero() {
        // CSI K (no param) is same as CSI 0K
        assert_eq!(
            strip_ansi_with_clear_detect("old text\r\u{1b}[Knew"),
            ("new".to_string(), false)
        );
    }

    #[test]
    fn csi_erase_line_from_start() {
        // CSI 1K: erase from start to cursor (fills with spaces)
        assert_eq!(
            strip_ansi_with_clear_detect("abcdef\r\u{1b}[3C\u{1b}[1K"),
            ("   def".to_string(), false)
        );
    }

    #[test]
    fn csi_erase_entire_line() {
        // CSI 2K: erase entire line
        assert_eq!(
            strip_ansi_with_clear_detect("something\r\u{1b}[2Kresult"),
            ("result".to_string(), false)
        );
    }

    #[test]
    fn csi_cursor_forward() {
        // CSI C: move cursor forward
        assert_eq!(
            strip_ansi_with_clear_detect("abcdef\r\u{1b}[3CX"),
            ("abcXef".to_string(), false)
        );
    }

    #[test]
    fn csi_cursor_backward() {
        // CSI D: move cursor backward
        assert_eq!(
            strip_ansi_with_clear_detect("abcdef\u{1b}[2DXY"),
            ("abcdXY".to_string(), false)
        );
    }

    #[test]
    fn csi_cursor_absolute_column() {
        // CSI G: absolute column positioning (1-based)
        assert_eq!(
            strip_ansi_with_clear_detect("abcdef\u{1b}[2GX"),
            ("aXcdef".to_string(), false)
        );
    }

    #[test]
    fn backspace_moves_cursor_back() {
        assert_eq!(
            strip_ansi_with_clear_detect("abc\x08X"),
            ("abX".to_string(), false)
        );
    }

    #[test]
    fn backspace_at_start_does_not_underflow() {
        assert_eq!(
            strip_ansi_with_clear_detect("\x08\x08hello"),
            ("hello".to_string(), false)
        );
    }

    #[test]
    fn claude_code_progress_pattern() {
        // Claude Code CLI pattern: write progress, \r, erase line, write new status
        let input = "⠋ Thinking...\r\u{1b}[K⠙ Analyzing...\r\u{1b}[K✓ Done";
        assert_eq!(
            strip_ansi_with_clear_detect(input),
            ("✓ Done".to_string(), false)
        );
    }

    #[test]
    fn unicode_overwrite_preserves_chars() {
        // CJK characters with cursor moves
        assert_eq!(
            strip_ansi_with_clear_detect("你好世界\r\u{1b}[2C再"),
            ("你好再界".to_string(), false)
        );
    }

    #[test]
    fn mixed_ansi_colors_stripped_correctly() {
        // Colored text with cursor movement should strip colors and handle cursor
        assert_eq!(
            strip_ansi_with_clear_detect("\u{1b}[32mhello\u{1b}[0m\rbye"),
            ("byelo".to_string(), false)
        );
    }

    #[test]
    fn clear_screen_still_detected() {
        // CSI 2J and 3J still trigger clear
        assert_eq!(
            strip_ansi_with_clear_detect("\u{1b}[2J"),
            ("".to_string(), true)
        );
        assert_eq!(
            strip_ansi_with_clear_detect("\u{1b}[3J"),
            ("".to_string(), true)
        );
        // CSI 0J / CSI 1J do not trigger clear
        assert_eq!(
            strip_ansi_with_clear_detect("\u{1b}[0J"),
            ("".to_string(), false)
        );
    }

    #[test]
    fn cursor_home_repaint_collapses_to_final_frame() {
        // A `top`-style repaint: each frame is drawn behind a cursor-home, so
        // only the final frame must survive rather than being concatenated.
        let frame = |n: char| format!("header {n}\nrow-a {n}\nrow-b {n}");
        let stream = format!("{}\u{1b}[H{}\u{1b}[H{}", frame('1'), frame('2'), frame('3'),);
        assert_eq!(strip_ansi(&stream), frame('3'));
    }

    #[test]
    fn absolute_positioning_overwrites_earlier_row() {
        // CSI <row>;<col>H writes into an existing row instead of appending.
        assert_eq!(
            strip_ansi("line1\nline2\nline3\u{1b}[2;1HLINE2"),
            "line1\nLINE2\nline3"
        );
    }

    #[test]
    fn cursor_up_rewrites_previous_line() {
        // Multi-line progress: go up one row and overwrite it.
        assert_eq!(
            strip_ansi("step 1: pending\nstep 2: pending\u{1b}[A\rstep 1: done   "),
            "step 1: done   \nstep 2: pending"
        );
    }

    #[test]
    fn erase_to_end_of_screen_drops_stale_rows() {
        // A shorter repaint clears the tail of a taller previous frame.
        assert_eq!(strip_ansi("aaa\nbbb\nccc\u{1b}[Hxxx\n\u{1b}[J"), "xxx\n");
    }

    #[test]
    fn vertical_repaint_detected_for_reposition_streams() {
        assert!(output_has_vertical_repaint("row1\nrow2\u{1b}[Hrepaint"));
        assert!(output_has_vertical_repaint("row1\nrow2\u{1b}[Arewrite"));
        assert!(output_has_vertical_repaint("row1\n\u{1b}[2;1Habsolute"));
    }

    #[test]
    fn collapse_repaint_merges_incremental_frames_and_trims_padding() {
        // top repaints incrementally: frame 1 paints the full screen, later
        // frames rewrite only changed cells. The merge must keep the untouched
        // rows; screen padding (trailing default-styled cells + blank rows) must
        // be trimmed, and rows are joined with CRLF for the finished VTE.
        let stream = concat!(
            "\u{1b}[H",
            "cpu  1%   \r\n", // row0, padded
            "mem  40%  \r\n", // row1, padded
            "static    \r\n", // row2, padded, never rewritten again
            "          \r\n", // row3, blank padding row
            "\u{1b}[H",       // next refresh: only rows 0 and 1 change
            "cpu  9%   \r\n",
            "mem  42%  ",
        );
        // No SGR in this stream, so the collapsed frame is plain text with CRLF.
        assert_eq!(
            collapse_repaint_output(stream, 10),
            "cpu  9%\r\nmem  42%\r\nstatic"
        );
    }

    #[test]
    fn collapse_repaint_preserves_color_and_reverse_bar() {
        // A reverse-video header that erases to end-of-line paints a full-width
        // bar (erase fills with the active background); colour must survive and
        // the bar extend to `cols`, while a plain row's padding is trimmed.
        let stream = concat!(
            "\u{1b}[H",
            "\u{1b}[7m PID\u{1b}[K\r\n", // reverse bar, erase-to-EOL fills reverse
            "\u{1b}[0m  1 root\u{1b}[K", // plain row, padding trimmed
        );
        let out = collapse_repaint_output(stream, 8);
        // Reverse attribute retained and the bar padded to 8 columns.
        assert_eq!(out, "\u{1b}[0;7m PID    \u{1b}[0m\r\n  1 root");
    }

    #[test]
    fn vertical_repaint_not_flagged_for_plain_or_horizontal_output() {
        // Leading home before any line, colored output, and CR spinners are not
        // repaint streams — they keep their raw (colored) bytes.
        assert!(!output_has_vertical_repaint(
            "\u{1b}[Hgit status output\nmore"
        ));
        assert!(!output_has_vertical_repaint(
            "\u{1b}[32mgreen\u{1b}[0m\nplain"
        ));
        assert!(!output_has_vertical_repaint("working\rdone\nnext line"));
        assert!(!output_has_vertical_repaint(
            "\u{1b}[?25lhidden cursor\nline"
        ));
    }

    #[test]
    fn running_root_handler_only_falls_back_for_interrupt_and_eof() {
        use gtk4::gdk::{Key, ModifierType};

        assert_eq!(
            super::running_root_control_bytes(Key::c, ModifierType::CONTROL_MASK),
            Some(b"\x03".as_slice())
        );
        assert_eq!(
            super::running_root_control_bytes(Key::D, ModifierType::CONTROL_MASK),
            Some(b"\x04".as_slice())
        );
        assert_eq!(
            super::running_root_control_bytes(Key::a, ModifierType::empty()),
            None
        );
        assert_eq!(
            super::running_root_control_bytes(Key::Return, ModifierType::empty()),
            None
        );
        assert_eq!(
            super::running_root_control_bytes(Key::BackSpace, ModifierType::empty()),
            None
        );
        assert_eq!(
            super::running_root_control_bytes(
                Key::c,
                ModifierType::CONTROL_MASK | ModifierType::ALT_MASK
            ),
            None
        );
    }

    // ── IME / Chinese input support tests ────────────────────────────────

    /// Simulate the logic from connect_commit: insert text at cursor position
    fn simulate_ime_commit(cmd: &str, cursor_pos: usize, committed: &str) -> (String, usize) {
        let mut buf = cmd.to_string();
        let byte_pos = buf
            .char_indices()
            .nth(cursor_pos)
            .map(|(i, _)| i)
            .unwrap_or(buf.len());
        buf.insert_str(byte_pos, committed);
        let new_pos = cursor_pos + committed.chars().count();
        (buf, new_pos)
    }

    #[test]
    fn ime_commit_chinese_at_end() {
        let (buf, pos) = simulate_ime_commit("ls ", 3, "你好");
        assert_eq!(buf, "ls 你好");
        assert_eq!(pos, 5);
    }

    #[test]
    fn ime_commit_chinese_at_beginning() {
        let (buf, pos) = simulate_ime_commit("hello", 0, "世界");
        assert_eq!(buf, "世界hello");
        assert_eq!(pos, 2);
    }

    #[test]
    fn ime_commit_chinese_in_middle() {
        let (buf, pos) = simulate_ime_commit("echo test", 5, "中文");
        assert_eq!(buf, "echo 中文test");
        assert_eq!(pos, 7);
    }

    #[test]
    fn ime_commit_after_existing_chinese() {
        let (buf, pos) = simulate_ime_commit("你好", 2, "世界");
        assert_eq!(buf, "你好世界");
        assert_eq!(pos, 4);
    }

    #[test]
    fn ime_commit_mixed_cjk_ascii() {
        let (buf, pos) = simulate_ime_commit("git commit -m \"", 15, "修复bug");
        assert_eq!(buf, "git commit -m \"修复bug");
        // 修复bug = 5 chars (修,复,b,u,g), so pos = 15 + 5 = 20
        assert_eq!(pos, 20);
    }

    #[test]
    fn ime_preedit_cursor_position() {
        // During composition, cursor should be after cmd + preedit
        let cmd = "echo ";
        let preedit = "niha"; // pinyin input not yet committed
        let cursor_pos = cmd.chars().count() + preedit.chars().count();
        assert_eq!(cursor_pos, 9);
    }

    #[test]
    fn ime_preedit_buffer_format() {
        // The display buffer format: "{cmd}{preedit} {suggestion}"
        let cmd = "echo ";
        let preedit = "你好";
        let suggestion = "";
        let text = format!("{}{} {}", cmd, preedit, suggestion);
        assert_eq!(text, "echo 你好 ");
        // Preedit tag range: cmd.chars().count() .. cmd.chars().count() + preedit.chars().count()
        let preedit_start = cmd.chars().count();
        let preedit_end = preedit_start + preedit.chars().count();
        assert_eq!(preedit_start, 5);
        assert_eq!(preedit_end, 7);
    }

    #[test]
    fn ime_commit_clears_preedit_state() {
        // After commit, preedit should be empty and cursor advances
        let cmd = "ls ";
        let _preedit = "zhong"; // composing
                                // Simulate commit of "中"
        let (buf, pos) = simulate_ime_commit(cmd, cmd.chars().count(), "中");
        assert_eq!(buf, "ls 中");
        assert_eq!(pos, 4);
        // preedit should be cleared (tested by set_preedit("") after commit)
        let final_preedit = "";
        let display = format!("{} {}", buf, final_preedit);
        assert_eq!(display, "ls 中 ");
    }

    #[test]
    fn ime_backspace_chinese_char() {
        // Backspace should delete one full CJK character
        let cmd = "你好世界";
        let pos = 4; // cursor at end
        let mut buf = cmd.to_string();
        let byte_pos = buf.char_indices().nth(pos - 1).map(|(i, _)| i).unwrap_or(0);
        let next_byte = buf
            .char_indices()
            .nth(pos)
            .map(|(i, _)| i)
            .unwrap_or(buf.len());
        buf.drain(byte_pos..next_byte);
        assert_eq!(buf, "你好世");
        assert_eq!(buf.chars().count(), 3);
    }

    #[test]
    fn ime_cursor_movement_with_chinese() {
        // Left/right should move by one char (not byte)
        let cmd = "你好world";
        let chars: Vec<char> = cmd.chars().collect();
        assert_eq!(chars.len(), 7); // 你好 = 2 chars, world = 5 chars
                                    // At pos 2, cursor is between '好' and 'w'
        let pos = 2;
        assert_eq!(chars[pos - 1], '好');
        assert_eq!(chars[pos], 'w');
    }
    #[test]
    fn clipboard_paste_payload_is_one_ordered_transaction() {
        assert_eq!(
            build_clipboard_paste("plain", false).bytes.as_slice(),
            b"plain"
        );
        assert_eq!(
            build_clipboard_paste("one\ntwo", true).bytes.as_slice(),
            b"\x1b[200~one\ntwo\x1b[201~"
        );
    }

    #[test]
    fn approved_command_and_enter_are_one_bounded_transaction() {
        assert_eq!(
            approved_command_submission_payload("printf safe").unwrap(),
            b"printf safe\r"
        );
        assert!(approved_command_submission_payload("printf one\nprintf two").is_err());
        assert!(
            approved_command_submission_payload(
                &"x".repeat(crate::pty::MAX_PTY_INPUT_MESSAGE_BYTES)
            )
            .is_err(),
            "the submit byte must fit inside the same bounded queue item"
        );
    }

    #[test]
    fn rejected_vte_enqueue_can_restore_shadow_without_per_key_full_clone() {
        let mut shadow = "printf 你".to_string();
        let rollback = vte_commit_shadow_rollback(&shadow, "好");
        apply_vte_commit_to_shadow(&mut shadow, "好");
        assert_eq!(shadow, "printf 你好");
        rollback.apply(&mut shadow);
        assert_eq!(shadow, "printf 你");

        let previous = "x".repeat(MAX_COMMAND_CAPTURE_BYTES - 1);
        let mut overflowing = previous.clone();
        let rollback = vte_commit_shadow_rollback(&overflowing, "界");
        apply_vte_commit_to_shadow(&mut overflowing, "界");
        assert_eq!(overflowing, TRUNCATED_COMMAND_PLACEHOLDER);
        rollback.apply(&mut overflowing);
        assert_eq!(overflowing, previous);
    }

    #[test]
    fn vte_shadow_ignores_c1_controls() {
        let mut shadow = "echo".to_string();
        apply_vte_commit_to_shadow(&mut shadow, "\u{0085}\u{009b}");
        assert_eq!(shadow, "echo");
    }

    #[test]
    fn selected_commands_preserve_terminal_order_and_skip_background_blocks() {
        let selected = HashSet::from([1_u64, 2, 3]);
        let text = selected_command_text(
            [
                (1, "printf one"),
                (2, ""),
                (3, "printf three"),
                (4, "not selected"),
            ],
            &selected,
        );
        assert_eq!(text, "printf one\nprintf three");
    }

    #[test]
    fn multiline_command_recall_is_bracketed_or_safely_reduced() {
        let paste = build_command_recall("printf one\r\nprintf two", true);
        assert_eq!(paste.echo_text, "printf one\nprintf two");
        // The unconditional Ctrl+U leads, then the frame.
        assert!(paste.bytes.starts_with(b"\x15\x1b[200~"));
        assert!(paste.bytes.ends_with(b"\x1b[201~"));

        let paste = build_command_recall("printf one\nprintf two", false);
        assert_eq!(paste.echo_text, "printf one");
        assert_eq!(paste.bytes, b"\x15printf one".to_vec());
        assert!(paste.risk.truncated_to_first_line);
    }

    #[test]
    fn captured_command_recall_strips_controls_and_rejects_visual_spoofing() {
        let paste = build_command_recall("echo \x1b[31mred", true);
        assert_eq!(paste.echo_text, "echo [31mred");
        assert!(paste.risk.had_controls);

        assert!(build_command_recall("echo safe\u{202e}txt", true).is_empty());
        assert!(build_command_recall(
            &"x".repeat(crate::review_input::MAX_REVIEW_INPUT_BYTES + 1),
            true
        )
        .is_empty());
    }
}
