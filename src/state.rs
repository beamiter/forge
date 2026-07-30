use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{Label, Notebook, Paned};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};
use vte4::Terminal;
use vte4::TerminalExt;

use crate::terminal::{
    find_first_terminal, terminal_child_lifecycle, terminal_child_pid, terminal_working_directory,
    TERMINAL_ESCALATION,
};
use crate::ui::{PaneLeaf, PaneNode};

const MAX_READY_WINDOW_STATES: usize = 32;
const MAX_WORKSPACE_STATE_BYTES: usize = 20 * 1024 * 1024;
const MAX_AI_METADATA_RESERVE_BYTES: usize = 64 * 1024;
const MAX_WINDOW_STATE_BYTES: usize = MAX_WORKSPACE_STATE_BYTES + MAX_AI_METADATA_RESERVE_BYTES;
const READY_STATE_EXTENSION: &str = "state";
const ACTIVE_STATE_EXTENSION: &str = "active";
const AI_CONVERSATION_PREFIX: &str = "ai_conversation=";
const MAX_AI_CONVERSATION_LINE_BYTES: usize = crate::ai::MAX_CONVERSATION_SNAPSHOT_JSON_BYTES * 2;

#[derive(Debug)]
struct WindowStatePaths {
    directory: PathBuf,
    active: PathBuf,
    ready: PathBuf,
}

static WINDOW_STATE_PATHS: OnceLock<WindowStatePaths> = OnceLock::new();
static WINDOW_STATE_FINALIZED: AtomicBool = AtomicBool::new(false);
static WINDOW_STATE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
static AI_CONVERSATION_SNAPSHOT: OnceLock<Mutex<Option<crate::ai::ConversationSnapshot>>> =
    OnceLock::new();

fn ai_conversation_slot() -> &'static Mutex<Option<crate::ai::ConversationSnapshot>> {
    AI_CONVERSATION_SNAPSHOT.get_or_init(|| Mutex::new(None))
}

/// Return the complete, bounded AI conversation associated with this process's
/// active window snapshot.
pub(crate) fn get_ai_conversation_snapshot() -> Option<crate::ai::ConversationSnapshot> {
    ai_conversation_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Replace the AI conversation that the next window-state save will embed.
/// Passing `None` durably removes the entire chat library from the snapshot.
pub(crate) fn set_ai_conversation_snapshot(snapshot: Option<crate::ai::ConversationSnapshot>) {
    *ai_conversation_slot()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot;
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(0o700);
    builder.create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn make_file_private(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

fn write_private_file(path: &Path, payload: &[u8]) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(payload)?;
    file.sync_all()
}

fn unique_state_temp_path(target: &Path) -> io::Result<PathBuf> {
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("state path has no parent: {}", target.display()),
        )
    })?;
    let file_name = target.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("state path has no file name: {}", target.display()),
        )
    })?;
    let sequence = WINDOW_STATE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut temp_name = OsString::from(".");
    temp_name.push(file_name);
    temp_name.push(format!(".tmp-{}-{sequence}", std::process::id()));
    Ok(parent.join(temp_name))
}

/// Durably replace a private state file without ever truncating the last good
/// snapshot. The sibling temporary file also prevents cross-filesystem rename.
fn atomic_write_private_file(target: &Path, payload: &[u8]) -> io::Result<()> {
    let temp_path = unique_state_temp_path(target)?;
    let result = (|| {
        write_private_file(&temp_path, payload)?;
        fs::rename(&temp_path, target)?;
        make_file_private(target)?;
        sync_parent_directory(target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn sync_parent_directory(path: &Path) -> io::Result<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::File::open(parent)?.sync_all()
}

fn window_state_directory() -> PathBuf {
    glib::user_config_dir().join("jterm4").join("windows")
}

fn legacy_tabs_state_file_path() -> PathBuf {
    glib::user_config_dir().join("jterm4").join("tabs.state")
}

fn window_state_paths() -> &'static WindowStatePaths {
    WINDOW_STATE_PATHS.get_or_init(|| {
        let directory = window_state_directory();
        let id = generate_session_id();
        WindowStatePaths {
            active: directory.join(format!("window-{id}.{ACTIVE_STATE_EXTENSION}")),
            ready: directory.join(format!("window-{id}.{READY_STATE_EXTENSION}")),
            directory,
        }
    })
}

pub(crate) fn tabs_state_file_path() -> PathBuf {
    window_state_paths().active.clone()
}

/// Generate a unique session ID for jsh session persistence and window-state files.
pub(crate) fn generate_session_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{}", std::process::id(), ts)
}

fn has_extension(path: &Path, extension: &str) -> bool {
    path.extension().and_then(|value| value.to_str()) == Some(extension)
}

fn modified_time(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(UNIX_EPOCH)
}

fn snapshots_with_extension(directory: &Path, extension: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut snapshots: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && has_extension(path, extension))
        .collect();
    snapshots.sort_by(|left, right| {
        modified_time(right)
            .cmp(&modified_time(left))
            .then_with(|| right.cmp(left))
    });
    snapshots
}

fn ready_snapshots_in(directory: &Path) -> Vec<PathBuf> {
    snapshots_with_extension(directory, READY_STATE_EXTENSION)
}

fn snapshot_owner_pid(path: &Path) -> Option<i32> {
    path.file_stem()?
        .to_str()?
        .strip_prefix("window-")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

fn recover_stale_active_snapshots(directory: &Path) {
    for active in snapshots_with_extension(directory, ACTIVE_STATE_EXTENSION) {
        if snapshot_owner_pid(&active).is_some_and(snapshot_owner_is_running) {
            continue;
        }
        let ready = active.with_extension(READY_STATE_EXTENSION);
        match fs::rename(&active, &ready) {
            Ok(()) => {
                if let Err(error) = make_file_private(&ready) {
                    log::warn!(
                        "Failed to tighten snapshot permissions {}: {error}",
                        ready.display()
                    );
                }
                log::info!("Recovered interrupted window snapshot {}", ready.display());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!(
                "Failed to recover interrupted window snapshot {}: {error}",
                active.display()
            ),
        }
    }
}

fn claim_ready_snapshot_in(directory: &Path, active: &Path) -> Option<PathBuf> {
    for candidate in ready_snapshots_in(directory) {
        match fs::rename(&candidate, active) {
            Ok(()) => return Some(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => log::debug!(
                "Failed to claim window snapshot {}: {error}",
                candidate.display()
            ),
        }
    }
    None
}

fn prune_ready_snapshots_in(directory: &Path, keep: usize) {
    for stale in ready_snapshots_in(directory).into_iter().skip(keep) {
        if let Err(error) = fs::remove_file(&stale) {
            log::debug!(
                "Failed to prune old window snapshot {}: {error}",
                stale.display()
            );
        }
    }
}

fn prepare_active_tabs_state_path() -> PathBuf {
    let paths = window_state_paths();
    if let Err(error) = ensure_private_directory(&paths.directory) {
        log::warn!(
            "Failed to create window-state directory {}: {error}",
            paths.directory.display()
        );
        return paths.active.clone();
    }

    recover_stale_active_snapshots(&paths.directory);
    if paths.active.exists() {
        return paths.active.clone();
    }

    // Upgrade the old single-file format first. Atomic rename means concurrent
    // launches cannot restore the same legacy snapshot.
    let legacy = legacy_tabs_state_file_path();
    if legacy.exists() {
        match fs::rename(&legacy, &paths.active) {
            Ok(()) => {
                if let Err(error) = make_file_private(&paths.active) {
                    log::warn!(
                        "Failed to tighten legacy snapshot permissions {}: {error}",
                        paths.active.display()
                    );
                }
                log::info!("Claimed legacy tabs snapshot {}", legacy.display());
                return paths.active.clone();
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => log::warn!(
                "Failed to claim legacy tabs snapshot {}: {error}",
                legacy.display()
            ),
        }
    }

    if let Some(claimed) = claim_ready_snapshot_in(&paths.directory, &paths.active) {
        if let Err(error) = make_file_private(&paths.active) {
            log::warn!(
                "Failed to tighten claimed snapshot permissions {}: {error}",
                paths.active.display()
            );
        }
        log::info!("Claimed window snapshot {}", claimed.display());
    }
    prune_ready_snapshots_in(&paths.directory, MAX_READY_WINDOW_STATES);
    paths.active.clone()
}

/// Report saved and currently active window snapshots without exposing paths.
pub(crate) fn session_snapshot_counts() -> (usize, usize) {
    let directory = window_state_directory();
    (
        ready_snapshots_in(&directory).len(),
        snapshots_with_extension(&directory, ACTIVE_STATE_EXTENSION).len(),
    )
}

/// Pane layout structure for serialization
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum PaneLayout {
    Leaf {
        dir: String,
        sid: String,
        /// Restorable command argv to replay on restore (e.g. `["ssh", "host"]`).
        /// Keeping it structured prevents shell metacharacters inside one
        /// argument from becoming a different local command after a restart.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "deserialize_restorable_argv"
        )]
        cmds: Option<Vec<String>>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pinned: Option<bool>,
    },
    Split {
        orientation: char, // 'h' or 'v'
        position: i32,
        start: Box<PaneLayout>,
        end: Box<PaneLayout>,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredRestorableCommand {
    Argv(Vec<String>),
    LegacyString(String),
}

/// Older snapshots stored a shell command string assembled with `argv.join`.
/// Its original argument boundaries cannot be recovered safely, so accept the
/// old shape for session compatibility but deliberately do not auto-execute it.
fn deserialize_restorable_argv<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let stored = Option::<StoredRestorableCommand>::deserialize(deserializer)?;
    Ok(match stored {
        Some(StoredRestorableCommand::Argv(argv)) if !argv.is_empty() => Some(argv),
        Some(StoredRestorableCommand::LegacyString(_)) => {
            log::debug!("Ignoring legacy session restore command without argv boundaries");
            None
        }
        _ => None,
    })
}

/// Serialize a pane layout tree from a GTK widget
pub(crate) fn serialize_pane_layout(
    widget: &gtk4::Widget,
    session_ids: &HashMap<u32, String>,
) -> PaneLayout {
    if let Some(paned) = widget.downcast_ref::<Paned>() {
        let orientation = match paned.orientation() {
            gtk4::Orientation::Horizontal => 'h',
            gtk4::Orientation::Vertical => 'v',
            _ => 'h',
        };

        let start = paned.start_child().expect("Paned must have start child");
        let end = paned.end_child().expect("Paned must have end child");

        PaneLayout::Split {
            orientation,
            position: paned.position(),
            start: Box::new(serialize_pane_layout(&start, session_ids)),
            end: Box::new(serialize_pane_layout(&end, session_ids)),
        }
    } else {
        // Leaf terminal
        let terminal = find_first_terminal(widget).expect("Leaf must contain terminal");
        let pane_leaf = PaneLeaf::from_widget(widget);
        let dir = pane_leaf
            .as_ref()
            .and_then(PaneLeaf::block_view)
            .map(|view| view.cwd())
            .filter(|cwd| !cwd.is_empty())
            .or_else(|| terminal_working_directory(&terminal))
            .unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".to_string()));

        // Prefer the identity attached to this exact leaf. The tab-number map
        // remains a compatibility fallback for older/top-level pages.
        let widget_name = widget.widget_name();
        let sid = pane_leaf
            .as_ref()
            .and_then(PaneLeaf::session_id)
            .unwrap_or_else(|| {
                if let Some(tab_str) = widget_name.to_string().strip_prefix("tab-") {
                    if let Ok(tab_num) = tab_str.parse::<u32>() {
                        session_ids
                            .get(&tab_num)
                            .cloned()
                            .unwrap_or_else(generate_session_id)
                    } else {
                        generate_session_id()
                    }
                } else {
                    generate_session_id()
                }
            });

        let cmds = pane_leaf
            .as_ref()
            .and_then(PaneLeaf::restorable_command)
            .or_else(|| get_restorable_commands(&terminal));

        // Check if this tab is pinned
        let pinned = unsafe { widget.data::<bool>("pinned").map(|p| *p.as_ref()) };

        PaneLayout::Leaf {
            dir,
            sid,
            cmds,
            pinned,
        }
    }
}

pub fn escape_tab_state(value: &str) -> String {
    // Optimized: single pass instead of multiple replace() calls
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            _ => out.push(ch),
        }
    }
    out
}

pub fn unescape_tab_state(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek().copied() {
                Some('t') => {
                    out.push('\t');
                    chars.next();
                }
                Some('n') => {
                    out.push('\n');
                    chars.next();
                }
                Some('\\') => {
                    out.push('\\');
                    chars.next();
                }
                _ => out.push(ch),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

fn ai_conversation_state_line(
    snapshot: &crate::ai::ConversationSnapshot,
) -> Result<String, crate::ai::ConversationSnapshotError> {
    let encoded = snapshot.to_json()?;
    let escaped = escape_tab_state(&encoded);
    if escaped.len() > MAX_AI_CONVERSATION_LINE_BYTES {
        return Err(crate::ai::ConversationSnapshotError::EncodedTooLarge);
    }
    Ok(format!("{AI_CONVERSATION_PREFIX}{escaped}"))
}

fn compact_ai_conversation_for_window(
    snapshot: &crate::ai::ConversationSnapshot,
    base_lines: &[String],
    max_workspace_bytes: usize,
    max_total_bytes: usize,
) -> Option<(String, crate::ai::ConversationSnapshot, bool)> {
    let base_len = window_state_payload_len(base_lines)?;
    if base_len > max_workspace_bytes {
        return None;
    }

    let line_separator = usize::from(!base_lines.is_empty());
    let mut compacted = snapshot.clone();
    compacted.compact_to_measured_limit(max_total_bytes, |candidate| {
        let line = ai_conversation_state_line(candidate).ok()?;
        base_len
            .checked_add(line_separator)?
            .checked_add(line.len())
    })?;
    let line = ai_conversation_state_line(&compacted).ok()?;
    let changed = compacted != *snapshot;
    Some((line, compacted, changed))
}

fn window_state_payload_len(lines: &[String]) -> Option<usize> {
    if lines.is_empty() {
        return Some(1);
    }
    lines.iter().try_fold(0usize, |total, line| {
        total.checked_add(line.len())?.checked_add(1)
    })
}

fn bounded_window_state_payload(lines: &[String], max_bytes: usize) -> Option<String> {
    let payload = lines.join("\n") + "\n";
    (payload.len() <= max_bytes).then_some(payload)
}

/// When the current workspace itself is too large to replace, preserve the
/// previous tab/pane payload but still refresh its optional AI line. This
/// keeps New chat and newly enabled redaction durable even at the workspace
/// size boundary.
fn rewrite_existing_ai_conversation(
    path: &Path,
    snapshot: Option<&crate::ai::ConversationSnapshot>,
) -> io::Result<bool> {
    let contents = read_window_state_bounded(path)?;
    let mut lines = Vec::new();
    let mut had_ai_line = false;
    for line in contents.lines() {
        if line.trim().starts_with(AI_CONVERSATION_PREFIX) {
            had_ai_line = true;
        } else {
            lines.push(line.to_string());
        }
    }

    if snapshot.is_none() && !had_ai_line {
        return Ok(false);
    }

    let mut compacted_ai = false;
    let mut durable_ai_snapshot = None;
    if let Some(snapshot) = snapshot {
        let (line, compacted_snapshot, compacted) = compact_ai_conversation_for_window(
            snapshot,
            &lines,
            MAX_WORKSPACE_STATE_BYTES,
            MAX_WINDOW_STATE_BYTES,
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "AI chat metadata cannot fit the preserved workspace snapshot",
            )
        })?;
        compacted_ai = compacted;
        durable_ai_snapshot = Some(compacted_snapshot);
        if compacted {
            log::warn!("Compacted AI chats to fit the preserved workspace snapshot");
        }
        let insertion = usize::from(
            lines
                .first()
                .is_some_and(|line| line.starts_with("current_page=")),
        );
        lines.insert(insertion, line);
    }

    let payload_limit = if snapshot.is_some() {
        MAX_WINDOW_STATE_BYTES
    } else {
        MAX_WORKSPACE_STATE_BYTES
    };
    let payload = bounded_window_state_payload(&lines, payload_limit).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "previous workspace snapshot cannot be safely rewritten",
        )
    })?;
    atomic_write_private_file(path, payload.as_bytes())?;
    if let Some(snapshot) = durable_ai_snapshot {
        set_ai_conversation_snapshot(Some(snapshot));
    }
    Ok(compacted_ai)
}

/// Parse the AI payload independently from tabs. Any malformed, duplicated,
/// unsupported, or oversized value is ignored without affecting tab recovery.
fn parse_ai_conversation(contents: &str) -> Option<crate::ai::ConversationSnapshot> {
    let mut parsed = None;
    let mut found = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        let Some(value) = line.strip_prefix(AI_CONVERSATION_PREFIX) else {
            continue;
        };
        if found {
            log::warn!("Ignoring duplicated AI conversation in window snapshot");
            return None;
        }
        found = true;
        if value.len() > MAX_AI_CONVERSATION_LINE_BYTES {
            log::warn!("Ignoring oversized AI conversation in window snapshot");
            return None;
        }
        let encoded = unescape_tab_state(value);
        match crate::ai::ConversationSnapshot::from_json(&encoded) {
            Ok(snapshot) => parsed = Some(snapshot),
            Err(error) => {
                log::warn!("Ignoring invalid AI conversation in window snapshot: {error}");
                return None;
            }
        }
    }
    parsed
}

pub fn parse_tabs_state(contents: &str) -> (Option<u32>, Vec<(Option<String>, PaneLayout)>) {
    let mut current_page: Option<u32> = None;
    let mut tabs: Vec<(Option<String>, PaneLayout)> = Vec::new();

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("current_page=") {
            current_page = rest.trim().parse::<u32>().ok();
            continue;
        }
        if let Some(rest) = line.strip_prefix("tab=") {
            // Split into fields
            let fields: Vec<&str> = rest.splitn(4, '\t').collect();
            match fields.len() {
                1 => {
                    // Just dir (legacy)
                    let dir = unescape_tab_state(fields[0]);
                    let layout = PaneLayout::Leaf {
                        dir,
                        sid: generate_session_id(),
                        cmds: None,
                        pinned: None,
                    };
                    tabs.push((None, layout));
                }
                2 => {
                    // New format: name + layout_json OR legacy: name + dir
                    let name = unescape_tab_state(fields[0]);
                    let data = unescape_tab_state(fields[1]);

                    // Try parsing as JSON first (new format)
                    if let Ok(layout) = serde_json::from_str::<PaneLayout>(&data) {
                        tabs.push((Some(name), layout));
                    } else {
                        // Legacy: treat as directory
                        let layout = PaneLayout::Leaf {
                            dir: data,
                            sid: generate_session_id(),
                            cmds: None,
                            pinned: None,
                        };
                        tabs.push((Some(name), layout));
                    }
                }
                3 => {
                    // Legacy: name + dir + session_id
                    let name = unescape_tab_state(fields[0]);
                    let dir = unescape_tab_state(fields[1]);
                    let sid = unescape_tab_state(fields[2]);
                    let effective_sid = if sid.is_empty() {
                        generate_session_id()
                    } else {
                        sid
                    };
                    let layout = PaneLayout::Leaf {
                        dir,
                        sid: effective_sid,
                        cmds: None,
                        pinned: None,
                    };
                    tabs.push((Some(name), layout));
                }
                4 => {
                    // Legacy: name + dir + session_id + commands. The old
                    // command field was a joined string whose argv boundaries
                    // cannot be recovered safely, so the tab loads but the
                    // command is never replayed.
                    let name = unescape_tab_state(fields[0]);
                    let dir = unescape_tab_state(fields[1]);
                    let sid = unescape_tab_state(fields[2]);
                    if !fields[3].is_empty() {
                        log::debug!(
                            "Ignoring legacy session restore command without argv boundaries"
                        );
                    }
                    let effective_sid = if sid.is_empty() {
                        generate_session_id()
                    } else {
                        sid
                    };
                    let layout = PaneLayout::Leaf {
                        dir,
                        sid: effective_sid,
                        cmds: None,
                        pinned: None,
                    };
                    tabs.push((Some(name), layout));
                }
                _ => {}
            }
            continue;
        }
        // Parsed separately so a damaged or future AI payload cannot create a
        // bogus legacy path tab or interfere with workspace recovery.
        if line.starts_with(AI_CONVERSATION_PREFIX) {
            continue;
        }
        // Legacy: bare path line
        let layout = PaneLayout::Leaf {
            dir: line.to_string(),
            sid: generate_session_id(),
            cmds: None,
            pinned: None,
        };
        tabs.push((None, layout));
    }

    (current_page, tabs)
}

fn read_window_state_bounded(path: &Path) -> io::Result<String> {
    let file = fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_WINDOW_STATE_BYTES as u64 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "window snapshot exceeds the {} byte limit",
                MAX_WINDOW_STATE_BYTES
            ),
        ));
    }
    let mut contents = String::new();
    file.take((MAX_WINDOW_STATE_BYTES + 1) as u64)
        .read_to_string(&mut contents)?;
    if contents.len() > MAX_WINDOW_STATE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "window snapshot exceeds the {} byte limit",
                MAX_WINDOW_STATE_BYTES
            ),
        ));
    }
    Ok(contents)
}

pub(crate) fn load_tabs_state() -> (Option<u32>, Vec<(Option<String>, PaneLayout)>) {
    let path = prepare_active_tabs_state_path();
    log::info!("Loading tabs state from: {}", path.display());

    let contents = match read_window_state_bounded(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            set_ai_conversation_snapshot(None);
            log::info!("No window snapshot found (first run or a new window)");
            return (None, Vec::new());
        }
        Err(error) => {
            set_ai_conversation_snapshot(None);
            log::warn!(
                "Ignoring unreadable window snapshot {}: {error}",
                path.display()
            );
            return (None, Vec::new());
        }
    };

    set_ai_conversation_snapshot(parse_ai_conversation(&contents));
    let (current_page, tabs) = parse_tabs_state(&contents);
    log::info!("Loaded {} tabs from window snapshot", tabs.len());
    (current_page, tabs)
}

/// Publish this process's active snapshot for a future jterm4 window. Active
/// snapshots are deliberately invisible to other running instances.
pub(crate) fn finalize_tabs_state() {
    if WINDOW_STATE_FINALIZED.swap(true, Ordering::AcqRel) {
        return;
    }

    let paths = window_state_paths();
    if !paths.active.exists() {
        return;
    }
    match fs::rename(&paths.active, &paths.ready) {
        Ok(()) => {
            if let Err(error) = make_file_private(&paths.ready) {
                log::warn!(
                    "Failed to tighten published snapshot permissions {}: {error}",
                    paths.ready.display()
                );
            }
            if let Err(error) = sync_parent_directory(&paths.ready) {
                log::debug!(
                    "Failed to sync window-state directory {}: {error}",
                    paths.directory.display()
                );
            }
            prune_ready_snapshots_in(&paths.directory, MAX_READY_WINDOW_STATES);
            log::info!("Published window snapshot {}", paths.ready.display());
        }
        Err(error) => log::error!(
            "Failed to publish window snapshot {}: {error}",
            paths.active.display()
        ),
    }
}

pub(crate) fn tab_label_text(notebook: &Notebook, widget: &gtk4::Widget) -> Option<String> {
    let tab_label = notebook.tab_label(widget)?;
    let tab_box = tab_label.downcast::<gtk4::Box>().ok()?;
    let first_child = tab_box.first_child()?;
    let label = first_child.downcast::<Label>().ok()?;
    Some(label.text().to_string())
}

/// Whether the window process that owns an interrupted snapshot is still
/// running.
///
/// Deliberately not a [`crate::process::ChildLifecycle`] question: this pid
/// belongs to another jterm4 window, never to a child of this process, so
/// nothing on this path may ever signal it — not even signal 0. A `/proc` probe
/// answers the only thing snapshot recovery asks, and anything short of a
/// definitely-vanished process counts as alive so a live window's snapshot is
/// never stolen.
fn snapshot_owner_is_running(pid: i32) -> bool {
    match crate::process::process_stat_result(pid) {
        Ok(_) => true,
        Err(error) => !matches!(
            error.kind(),
            std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
        ),
    }
}

pub(crate) fn kill_widget_child_processes(widget: &gtk4::Widget) -> bool {
    if let Some(node) = PaneNode::from_widget(widget) {
        for leaf in node.leaves() {
            leaf.kill();
        }
        return true;
    }
    false
}

/// Terminate a terminal child process and everything it dragged into its PTY
/// session before the UI tears down.
///
/// The lifecycle attached to the widget carries who reaps that child — VTE's
/// glib child watch for a conventional pane, this process for a Block pane —
/// so this path never has to guess from the widget type. Calling it a second
/// time (an explicit close followed by the drop of the same pane) is a no-op.
pub(crate) fn kill_terminal_child(terminal: &Terminal) {
    let Some(lifecycle) = terminal_child_lifecycle(terminal) else {
        return;
    };
    lifecycle.terminate(TERMINAL_ESCALATION);
}

/// Send SIGHUP to all child process groups across every terminal in the notebook.
pub(crate) fn kill_all_terminal_children(notebook: &Notebook) {
    for i in 0..notebook.n_pages() {
        if let Some(page_widget) = notebook.nth_page(Some(i)) {
            let _ = kill_widget_child_processes(&page_widget);
        }
    }
}

/// Conventional-VTE compatibility wrapper. Block panes must use their
/// `PaneLeaf` probe because their custom PTY is intentionally not VTE-owned.
pub(crate) fn get_restorable_commands(terminal: &Terminal) -> Option<Vec<String>> {
    let shell_pid = terminal_child_pid(terminal)?;
    let pty_fd = terminal.pty()?.fd().as_raw_fd();
    crate::process::restorable_command(pty_fd, shell_pid)
}

/// Conventional-VTE compatibility wrapper for tooltip callers.
pub(crate) fn get_foreground_process_name(terminal: &Terminal) -> Option<String> {
    let shell_pid = terminal_child_pid(terminal)?;
    let pty_fd = terminal.pty()?.fd().as_raw_fd();
    crate::process::foreground_process_name(pty_fd, shell_pid)
}

pub(crate) fn save_tabs_state(notebook: &Notebook, session_ids: &HashMap<u32, String>) {
    if WINDOW_STATE_FINALIZED.load(Ordering::Acquire) {
        return;
    }
    let path = tabs_state_file_path();
    log::info!("Saving tabs state to: {}", path.display());

    if let Some(parent) = path.parent() {
        if let Err(err) = ensure_private_directory(parent) {
            log::error!("Failed to create state dir {}: {err}", parent.display());
            return;
        }
    }

    let _home = std::env::var("HOME").ok();
    let n_pages = notebook.n_pages();
    log::info!("Saving {} tabs", n_pages);
    let mut lines: Vec<String> = Vec::with_capacity((n_pages as usize) + 2);
    if let Some(current) = notebook.current_page() {
        lines.push(format!("current_page={current}"));
    }
    let ai_snapshot = get_ai_conversation_snapshot();

    for i in 0..n_pages {
        let Some(widget) = notebook.nth_page(Some(i)) else {
            continue;
        };

        let label_text =
            tab_label_text(notebook, &widget).unwrap_or_else(|| format!("Terminal {}", i + 1));

        // Serialize the pane layout (supports splits)
        let layout = serialize_pane_layout(&widget, session_ids);
        let layout_json = serde_json::to_string(&layout).unwrap_or_else(|e| {
            log::error!("Failed to serialize layout: {}", e);
            "{}".to_string()
        });

        let line = format!(
            "tab={}\t{}",
            escape_tab_state(&label_text),
            escape_tab_state(&layout_json)
        );
        lines.push(line);
    }

    if window_state_payload_len(&lines).is_none_or(|length| length > MAX_WORKSPACE_STATE_BYTES) {
        log::error!(
            "Refusing to save window snapshot because tabs and panes exceed the {} byte limit",
            MAX_WORKSPACE_STATE_BYTES
        );
        match rewrite_existing_ai_conversation(&path, ai_snapshot.as_ref()) {
            Ok(true) => log::warn!(
                "Preserved the previous workspace snapshot and compacted its AI conversation"
            ),
            Ok(false) => log::warn!(
                "Preserved the previous workspace snapshot and refreshed its AI conversation"
            ),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                log::debug!("No previous workspace snapshot exists to refresh")
            }
            Err(error) => log::error!(
                "Failed to refresh AI state in previous workspace snapshot {}: {error}",
                path.display()
            ),
        }
        return;
    }

    let mut has_ai_line = false;
    let mut durable_ai_snapshot = None;
    if let Some(snapshot) = ai_snapshot.as_ref() {
        match compact_ai_conversation_for_window(
            snapshot,
            &lines,
            MAX_WORKSPACE_STATE_BYTES,
            MAX_WINDOW_STATE_BYTES,
        ) {
            Some((line, compacted_snapshot, compacted)) => {
                if compacted {
                    log::warn!(
                        "Compacted older AI chat content to fit the complete window snapshot"
                    );
                }
                let insertion = usize::from(
                    lines
                        .first()
                        .is_some_and(|line| line.starts_with("current_page=")),
                );
                lines.insert(insertion, line);
                has_ai_line = true;
                durable_ai_snapshot = Some(compacted_snapshot);
            }
            None => {
                log::error!(
                    "Refusing to replace the window snapshot because valid AI chat metadata cannot fit its reserved budget"
                );
                return;
            }
        }
    }

    let payload_limit = if has_ai_line {
        MAX_WINDOW_STATE_BYTES
    } else {
        MAX_WORKSPACE_STATE_BYTES
    };
    let Some(payload) = bounded_window_state_payload(&lines, payload_limit) else {
        log::error!(
            "Refusing to replace the window snapshot because its measured payload exceeds the {} byte limit",
            payload_limit
        );
        return;
    };

    // Write, fsync, atomically replace, then fsync the directory. A failure at
    // any earlier stage leaves the last good active snapshot untouched.
    if let Err(err) = atomic_write_private_file(&path, payload.as_bytes()) {
        log::error!(
            "Failed to atomically save state file {}: {err}",
            path.display()
        );
        return;
    }
    if let Some(snapshot) = durable_ai_snapshot {
        set_ai_conversation_snapshot(Some(snapshot));
    }
    log::info!("Successfully saved tabs state to {}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation_snapshot(question: &str, answer: &str) -> crate::ai::ConversationSnapshot {
        let history = vec![
            crate::ai::Turn {
                role: crate::ai::Role::User,
                text: question.into(),
            },
            crate::ai::Turn {
                role: crate::ai::Role::Assistant,
                text: answer.into(),
            },
        ];
        crate::ai::ConversationSnapshot::from_completed_history(&history, None).unwrap()
    }

    fn temporary_state_dir(test_name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("jterm4-{test_name}-{}", generate_session_id()));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn parses_snapshot_owner_pid() {
        assert_eq!(
            snapshot_owner_pid(Path::new("window-123-456.active")),
            Some(123)
        );
        assert_eq!(snapshot_owner_pid(Path::new("other.active")), None);
    }

    /// Snapshot recovery reclaims a file only from a window process that is
    /// definitely gone. The probe reads `/proc` and never signals: this pid
    /// belongs to another window, not to a child of this process.
    #[test]
    fn snapshot_recovery_only_reclaims_a_vanished_owner() {
        assert!(snapshot_owner_is_running(std::process::id() as i32));
        // Ill-formed owners parsed out of a file name must not read as alive.
        assert!(!snapshot_owner_is_running(0));
        assert!(!snapshot_owner_is_running(-1));

        let mut child = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn test child");
        let pid = child.id() as i32;
        child.wait().expect("reap test child");
        assert!(
            !snapshot_owner_is_running(pid),
            "a reaped process must read as gone so its snapshot can be recovered"
        );
    }

    #[test]
    fn claims_each_ready_snapshot_at_most_once() {
        let directory = temporary_state_dir("claim-ready");
        fs::write(directory.join("window-1-1.state"), "one").unwrap();
        fs::write(directory.join("window-2-2.state"), "two").unwrap();

        let active_one = directory.join("window-10-10.active");
        let active_two = directory.join("window-11-11.active");
        assert!(claim_ready_snapshot_in(&directory, &active_one).is_some());
        assert!(claim_ready_snapshot_in(&directory, &active_two).is_some());
        assert!(
            claim_ready_snapshot_in(&directory, &directory.join("window-12-12.active")).is_none()
        );

        let mut payloads = vec![
            fs::read_to_string(active_one).unwrap(),
            fs::read_to_string(active_two).unwrap(),
        ];
        payloads.sort();
        assert_eq!(payloads, ["one", "two"]);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn never_claims_an_active_window_snapshot() {
        let directory = temporary_state_dir("ignore-active");
        fs::write(directory.join("window-1-1.active"), "live").unwrap();
        let destination = directory.join("window-2-2.active");
        assert!(claim_ready_snapshot_in(&directory, &destination).is_none());
        assert!(!destination.exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn prunes_ready_snapshots_to_retention_limit() {
        let directory = temporary_state_dir("prune-ready");
        for index in 0..5 {
            fs::write(
                directory.join(format!("window-{index}-{index}.state")),
                index.to_string(),
            )
            .unwrap();
        }
        prune_ready_snapshots_in(&directory, 2);
        assert_eq!(ready_snapshots_in(&directory).len(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn private_state_storage_uses_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let root = temporary_state_dir("private-permissions");
        let directory = root.join("windows");
        let snapshot = directory.join("window-1-1.active");
        ensure_private_directory(&directory).unwrap();
        write_private_file(&snapshot, b"state").unwrap();

        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ai_conversation_line_round_trips_without_becoming_a_tab() {
        let snapshot = conversation_snapshot("为什么？\n第二行", "因为 C:\\tmp");
        let line = ai_conversation_state_line(&snapshot).unwrap();
        let contents = format!("current_page=0\n{line}\ntab=/tmp\n");

        assert_eq!(parse_ai_conversation(&contents), Some(snapshot));
        let (current_page, tabs) = parse_tabs_state(&contents);
        assert_eq!(current_page, Some(0));
        assert_eq!(tabs.len(), 1);
    }

    #[test]
    fn window_headroom_compacts_chat_payload_without_dropping_metadata() {
        let snapshot = conversation_snapshot(&"q".repeat(4096), &"a".repeat(4096));
        let mut base_lines = vec!["current_page=0".to_string(), "tab=/tmp".to_string()];
        let base_len = base_lines.iter().map(String::len).sum::<usize>() + base_lines.len();
        let max_bytes = base_len + 512;

        let (line, compacted, changed) =
            compact_ai_conversation_for_window(&snapshot, &base_lines, base_len, max_bytes)
                .unwrap();
        assert!(changed);
        assert_eq!(compacted.chats().len(), 1);
        assert!(compacted.active_chat().unwrap().turns().is_empty());
        assert!(compacted.active_chat().unwrap().history_truncated());

        base_lines.insert(1, line.clone());
        let payload = base_lines.join("\n") + "\n";
        assert!(payload.len() <= max_bytes);
        assert_eq!(parse_ai_conversation(&line), Some(compacted));
    }

    #[test]
    fn metadata_reserve_keeps_fifty_worst_case_chat_rows() {
        let first_id = u64::MAX - (crate::ai::MAX_PERSISTED_CHATS as u64 - 1);
        let title = "\\".repeat(80);
        let draft = "d".repeat(64 * 1024);
        let chats: Vec<_> = (0..crate::ai::MAX_PERSISTED_CHATS)
            .map(|offset| {
                crate::ai::ChatSnapshot::from_completed_history(
                    first_id + offset as u64,
                    &title,
                    offset % 2 == 0,
                    &[],
                    None,
                    &draft,
                )
            })
            .collect();
        let snapshot = crate::ai::ConversationSnapshot::from_chats(u64::MAX, chats).unwrap();
        let workspace_limit = 1024;
        let mut base_lines = vec!["x".repeat(workspace_limit - 1)];

        let (line, compacted, changed) = compact_ai_conversation_for_window(
            &snapshot,
            &base_lines,
            workspace_limit,
            workspace_limit + MAX_AI_METADATA_RESERVE_BYTES,
        )
        .unwrap();
        assert!(changed);
        assert_eq!(compacted.active_chat_id(), u64::MAX);
        assert_eq!(compacted.chats().len(), crate::ai::MAX_PERSISTED_CHATS);
        assert!(compacted.chats().iter().all(|chat| chat.title() == title));
        assert!(compacted.chats().iter().all(|chat| chat.draft().is_empty()));
        assert!(compacted
            .chats()
            .iter()
            .all(crate::ai::ChatSnapshot::history_truncated));

        base_lines.push(line);
        assert!(
            (base_lines.join("\n") + "\n").len() <= workspace_limit + MAX_AI_METADATA_RESERVE_BYTES
        );

        let oversized_base = vec!["x".repeat(workspace_limit)];
        assert!(compact_ai_conversation_for_window(
            &snapshot,
            &oversized_base,
            workspace_limit,
            workspace_limit + MAX_AI_METADATA_RESERVE_BYTES,
        )
        .is_none());
    }

    #[test]
    fn invalid_ai_payload_does_not_prevent_tab_recovery() {
        let contents = concat!(
            "ai_conversation={not-json}\n",
            "current_page=0\n",
            "tab=Terminal 1\t/tmp\t123-456\n"
        );

        assert!(parse_ai_conversation(contents).is_none());
        let (current_page, tabs) = parse_tabs_state(contents);
        assert_eq!(current_page, Some(0));
        assert_eq!(tabs.len(), 1);
    }

    #[test]
    fn duplicate_or_future_ai_payload_is_ignored() {
        let snapshot = conversation_snapshot("q", "a");
        let line = ai_conversation_state_line(&snapshot).unwrap();
        assert!(parse_ai_conversation(&format!("{line}\n{line}\n")).is_none());

        let future = r#"ai_conversation={"version":3,"active_chat_id":1,"chats":[]}"#;
        assert!(parse_ai_conversation(future).is_none());
    }

    #[test]
    fn stale_active_recovery_keeps_conversation_with_its_window() {
        let directory = temporary_state_dir("recover-ai");
        let snapshot = conversation_snapshot("crash question", "last complete answer");
        let line = ai_conversation_state_line(&snapshot).unwrap();
        let stale = directory.join(format!("window-{}-1.active", i32::MAX));
        fs::write(&stale, format!("{line}\ntab=/tmp\n")).unwrap();

        recover_stale_active_snapshots(&directory);
        let ready = stale.with_extension(READY_STATE_EXTENSION);
        assert!(ready.exists());
        let claimed = directory.join("window-10-10.active");
        assert!(claim_ready_snapshot_in(&directory, &claimed).is_some());
        let contents = fs::read_to_string(claimed).unwrap();
        assert_eq!(parse_ai_conversation(&contents), Some(snapshot));
        assert_eq!(parse_tabs_state(&contents).1.len(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn atomic_private_replace_is_durable_and_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = temporary_state_dir("atomic-replace");
        let directory = root.join("windows");
        ensure_private_directory(&directory).unwrap();
        let target = directory.join("window-1-1.active");
        atomic_write_private_file(&target, b"first").unwrap();
        atomic_write_private_file(&target, b"second").unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"second");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(fs::read_dir(&directory).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp-")
        }));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_reader_rejects_pathological_snapshot_before_parsing() {
        let root = temporary_state_dir("bounded-read");
        let path = root.join("oversized.state");
        let file = fs::File::create(&path).unwrap();
        file.set_len((MAX_WINDOW_STATE_BYTES + 1) as u64).unwrap();
        drop(file);

        let error = read_window_state_bounded(&path).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bounded_payload_refuses_oversize_instead_of_dropping_ai() {
        let lines = vec![
            "current_page=0".to_string(),
            "ai_conversation=xxxxxxxxxxxxxxxx".to_string(),
            "tab=workspace".to_string(),
        ];
        assert!(bounded_window_state_payload(&lines, 40).is_none());
        assert_eq!(lines[1], "ai_conversation=xxxxxxxxxxxxxxxx");
    }

    #[test]
    fn fallback_rewrites_ai_without_touching_previous_workspace() {
        let root = temporary_state_dir("rewrite-ai-only");
        let path = root.join("window-1-1.active");
        let original = conversation_snapshot("old question", "old answer");
        let replacement = conversation_snapshot("new question", "new answer");
        let original_line = ai_conversation_state_line(&original).unwrap();
        fs::write(
            &path,
            format!("current_page=0\n{original_line}\ntab=/tmp\n"),
        )
        .unwrap();

        assert!(!rewrite_existing_ai_conversation(&path, Some(&replacement)).unwrap());
        let replaced = fs::read_to_string(&path).unwrap();
        assert_eq!(parse_ai_conversation(&replaced), Some(replacement));
        assert_eq!(parse_tabs_state(&replaced).1.len(), 1);

        assert!(!rewrite_existing_ai_conversation(&path, None).unwrap());
        let cleared = fs::read_to_string(&path).unwrap();
        assert!(parse_ai_conversation(&cleared).is_none());
        assert_eq!(parse_tabs_state(&cleared).1.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
