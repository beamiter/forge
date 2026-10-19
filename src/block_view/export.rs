//! export — extracted from block_view (mechanical split, no logic changes)
//!
//! Serializes finished blocks to JSON / Markdown for the user-facing export
//! actions, plus a clipboard-copy helper for the per-block right-click menu.
//! Reads the completed-record snapshot owned by the active render backend; no
//! terminal state mutation other than the explicit clipboard helper.

use gtk4::glib;
use gtk4::prelude::*;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::{
    markdown_fence, BackendRecordRef, BackendRecords, CompletedCommandRecord, TermView,
    ZoneOutputSnapshot, MAX_ZONE_SNAPSHOT_BYTES,
};

#[derive(serde::Serialize)]
struct MetadataRecordExport<'a> {
    id: u64,
    cmd: &'a str,
    exit_code: Option<i32>,
    start_time_ms: Option<u64>,
    end_time_ms: Option<u64>,
    duration_ms: Option<u64>,
    cwd: Option<&'a str>,
    is_background: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_provenance: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lifecycle_health: Option<&'a str>,
    /// The bounded finalize-time snapshot. Omitted — never an empty string —
    /// when no snapshot is retained (none was captured, or the global budget
    /// evicted it).
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<&'a str>,
    /// `Some` exactly when `output` is; `true` when the snapshot holds only
    /// the bounded tail of the command's output.
    #[serde(skip_serializing_if = "Option::is_none")]
    output_truncated: Option<bool>,
    output_available: bool,
}

fn metadata_record_export<'a>(
    record: &'a CompletedCommandRecord,
    snapshot: Option<&'a ZoneOutputSnapshot>,
) -> MetadataRecordExport<'a> {
    MetadataRecordExport {
        id: record.id,
        cmd: &record.cmd,
        exit_code: record.exit_code,
        start_time_ms: record
            .timing_is_authoritative()
            .then_some(record.start_time_ms)
            .flatten(),
        end_time_ms: record
            .timing_is_authoritative()
            .then_some(record.end_time_ms)
            .flatten(),
        duration_ms: record
            .timing_is_authoritative()
            .then_some(record.duration_ms)
            .flatten(),
        cwd: record.cwd.as_deref(),
        is_background: record.is_background,
        completion_provenance: (!record.is_background)
            .then_some(record.completion_provenance.schema_name()),
        lifecycle_health: (!record.is_background)
            .then_some(record.lifecycle_health().schema_name()),
        output: snapshot.map(|snapshot| snapshot.plain.as_str()),
        output_truncated: snapshot.map(|snapshot| snapshot.truncated),
        output_available: snapshot.is_some(),
    }
}

fn metadata_record_markdown(
    record: &CompletedCommandRecord,
    snapshot: Option<&ZoneOutputSnapshot>,
) -> String {
    let mut markdown = if record.is_background {
        "## Background Output\n\n".to_string()
    } else {
        let fence = markdown_fence(&record.cmd);
        format!(
            "## Command Record\n\n**Command:**\n{fence}bash\n{}\n{fence}\n\n",
            record.cmd
        )
    };
    match snapshot {
        Some(snapshot) => {
            markdown.push_str("**Output:**");
            if snapshot.truncated {
                markdown.push_str(&format!(
                    " (truncated to the last {} KiB)",
                    MAX_ZONE_SNAPSHOT_BYTES / 1024
                ));
            }
            let fence = markdown_fence(&snapshot.plain);
            markdown.push_str(&format!("\n{fence}\n{}\n{fence}\n\n", snapshot.plain));
        }
        None => markdown.push_str(
            "**Output:** unavailable (retained on the live Unified terminal surface only)\n\n",
        ),
    }
    if !record.is_background {
        match record.exit_code {
            Some(code) => markdown.push_str(&format!("**Exit Code:** {code}\n\n")),
            None => markdown.push_str("**Exit Code:** unknown (the shell reported none)\n\n"),
        }
        markdown.push_str(&format!(
            "**Lifecycle:** {} ({})\n\n",
            record.lifecycle_health().schema_name(),
            record.completion_provenance.schema_name(),
        ));
    }
    if let Some(duration_ms) = record
        .timing_is_authoritative()
        .then_some(record.duration_ms)
        .flatten()
    {
        markdown.push_str(&format!(
            "**Duration:** {:.3}s\n\n",
            duration_ms as f64 / 1_000.0
        ));
    }
    markdown
}

fn record_json(record: BackendRecordRef<'_>) -> String {
    match record {
        BackendRecordRef::Block(record) => record.to_json(),
        BackendRecordRef::Metadata { record, snapshot } => {
            serde_json::to_string_pretty(&metadata_record_export(record, snapshot))
                .unwrap_or_else(|_| "{}".to_string())
        }
    }
}

fn record_markdown(record: BackendRecordRef<'_>) -> String {
    match record {
        BackendRecordRef::Block(record) => record.to_markdown(),
        BackendRecordRef::Metadata { record, snapshot } => {
            metadata_record_markdown(record, snapshot)
        }
    }
}

fn records_json(records: &BackendRecords<'_>) -> String {
    match records {
        BackendRecords::Blocks(records) => {
            serde_json::to_string_pretty(&**records).unwrap_or_else(|_| "[]".to_string())
        }
        BackendRecords::Metadata(store) => {
            let exports: Vec<_> = store
                .records
                .iter()
                .map(|record| metadata_record_export(record, store.snapshot(record.id)))
                .collect();
            serde_json::to_string_pretty(&exports).unwrap_or_else(|_| "[]".to_string())
        }
    }
}

/// On-disk formats for whole-session export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SessionExportFormat {
    Markdown,
    Json,
}

impl SessionExportFormat {
    fn extension(self) -> &'static str {
        match self {
            SessionExportFormat::Markdown => "md",
            SessionExportFormat::Json => "json",
        }
    }
}

pub(super) const MAX_SELECTED_RECORDS: usize = 64;
const MAX_SELECTED_EXPORT_BYTES: usize = 8 * 1024 * 1024;

/// Resolve exact identities in terminal order. A removed selection is never
/// replaced by its neighbour or silently omitted from an otherwise valid file.
pub(super) fn selected_records<'a>(
    records: &'a BackendRecords<'_>,
    selected: &HashSet<u64>,
) -> io::Result<Vec<BackendRecordRef<'a>>> {
    if selected.is_empty() || selected.len() > MAX_SELECTED_RECORDS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Select between 1 and 64 retained command records.",
        ));
    }
    let found: Vec<_> = records
        .iter()
        .filter(|record| selected.contains(&record.id()))
        .collect();
    if found.len() != selected.len() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "A selected record is no longer retained. Reopen review and select again.",
        ));
    }
    Ok(found)
}

/// A retained output can be a tail or prefix. Never infer completeness just
/// from a nonempty String, and never turn an unavailable snapshot into "".
pub(super) fn output_is_partial(record: BackendRecordRef<'_>) -> bool {
    match record {
        BackendRecordRef::Block(record) => record
            .output_notice
            .as_deref()
            .and_then(super::known_output_notice)
            .is_some(),
        BackendRecordRef::Metadata { snapshot, .. } => {
            snapshot.is_some_and(|snapshot| snapshot.truncated)
        }
    }
}

#[derive(serde::Serialize)]
struct SelectedRecordExport<'a> {
    id: u64,
    cmd: &'a str,
    cwd: Option<&'a str>,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
    command_exact: bool,
    command_truncated: bool,
    completion_provenance: &'static str,
    lifecycle_health: &'static str,
    is_background: bool,
    output_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_truncated: Option<bool>,
}

impl<'a> From<BackendRecordRef<'a>> for SelectedRecordExport<'a> {
    fn from(record: BackendRecordRef<'a>) -> Self {
        let (command_exact, command_truncated, completion_provenance, lifecycle_health) =
            match record {
                BackendRecordRef::Block(block) => (
                    block.command_exact,
                    block.command_truncated,
                    super::CompletionProvenance::from(block.completion_provenance).schema_name(),
                    block.lifecycle_health().schema_name(),
                ),
                BackendRecordRef::Metadata { record, .. } => (
                    record.command_source == super::CommandTextSource::ShellReported,
                    record.command_source == super::CommandTextSource::ScreenAfterTruncation,
                    record.completion_provenance.schema_name(),
                    record.lifecycle_health().schema_name(),
                ),
            };
        let duration_ms = match record {
            BackendRecordRef::Block(record) => record
                .timing_is_authoritative()
                .then_some(record.duration_ms)
                .flatten(),
            BackendRecordRef::Metadata { record, .. } => record
                .timing_is_authoritative()
                .then_some(record.duration_ms)
                .flatten(),
        };
        let output = record.output();
        Self {
            id: record.id(),
            cmd: record.command(),
            cwd: record.cwd(),
            exit_code: record.exit_code(),
            duration_ms,
            command_exact,
            command_truncated,
            completion_provenance,
            lifecycle_health,
            is_background: record.is_background(),
            output_available: output.is_some(),
            output,
            output_truncated: output.map(|_| output_is_partial(record)),
        }
    }
}

struct BoundedExport(Vec<u8>);

impl Write for BoundedExport {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_SELECTED_EXPORT_BYTES.saturating_sub(self.0.len()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Selected export exceeds 8 MiB. Select fewer records; nothing was written.",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn selected_export_document(
    records: &BackendRecords<'_>,
    selected: &HashSet<u64>,
    format: SessionExportFormat,
) -> io::Result<String> {
    let records = selected_records(records, selected)?;
    let exports: Vec<_> = records
        .into_iter()
        .map(SelectedRecordExport::from)
        .collect();
    let source_bytes = exports.iter().fold(0usize, |total, record| {
        total
            .saturating_add(record.cmd.len())
            .saturating_add(record.cwd.map_or(0, str::len))
            .saturating_add(record.output.map_or(0, str::len))
    });
    if source_bytes > MAX_SELECTED_EXPORT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Selected export exceeds 8 MiB. Select fewer records; nothing was written.",
        ));
    }
    let mut out = BoundedExport(Vec::new());
    match format {
        SessionExportFormat::Json => {
            #[derive(serde::Serialize)]
            struct Document<'a> {
                schema_version: u8,
                scope: &'static str,
                order: &'static str,
                records: &'a [SelectedRecordExport<'a>],
            }
            serde_json::to_writer_pretty(
                &mut out,
                &Document {
                    schema_version: 1,
                    scope: "selected_records",
                    order: "oldest_to_newest",
                    records: &exports,
                },
            )
            .map_err(io::Error::other)?;
        }
        SessionExportFormat::Markdown => {
            writeln!(out, "# Selected Command Records\n")?;
            writeln!(
                out,
                "{} selected records · oldest to newest · retained output only\n",
                exports.len()
            )?;
            for record in exports {
                writeln!(out, "## Record #{}\n", record.id)?;
                if record.is_background {
                    writeln!(out, "Background output (no command result).\n")?;
                } else {
                    let fence = markdown_fence(record.cmd);
                    writeln!(out, "**Command:**\n{fence}sh\n{}\n{fence}\n", record.cmd)?;
                    match record.exit_code {
                        Some(code) => writeln!(out, "**Exit code:** {code}\n")?,
                        None => writeln!(out, "**Exit code:** unknown\n")?,
                    }
                    let capture = if record.command_truncated {
                        "truncated command report"
                    } else if record.command_exact {
                        "shell-reported command"
                    } else {
                        "best-effort capture"
                    };
                    writeln!(out, "**Command capture:** {capture}\n")?;
                    writeln!(
                        out,
                        "**Lifecycle:** {} ({})\n",
                        record.lifecycle_health, record.completion_provenance
                    )?;
                }
                if let Some(cwd) = record.cwd {
                    let fence = markdown_fence(cwd);
                    writeln!(out, "**Working directory:**\n{fence}\n{cwd}\n{fence}\n")?;
                }
                if let Some(duration) = record.duration_ms {
                    writeln!(out, "**Duration:** {duration} ms\n")?;
                }
                match record.output {
                    Some(output) => {
                        if record.output_truncated == Some(true) {
                            writeln!(out, "**Output:** partial retained text (a tail or prefix; the full output is unavailable).\n")?;
                        } else {
                            writeln!(out, "**Output:** retained text\n")?;
                        }
                        let fence = markdown_fence(output);
                        writeln!(out, "{fence}\n{output}\n{fence}\n")?;
                    }
                    None => writeln!(out, "**Output:** unavailable (no retained snapshot).\n")?,
                }
            }
        }
    }
    String::from_utf8(out.0).map_err(io::Error::other)
}

/// `session-<stamp>.<ext>` with a numeric suffix for same-second collisions.
/// Same shape as anvil's exports so a user who runs both terminals gets one
/// recognizable naming scheme in their data directory.
fn export_file_name(stamp: &str, extension: &str, attempt: u32) -> String {
    if attempt == 0 {
        format!("session-{stamp}.{extension}")
    } else {
        format!("session-{stamp}-{attempt}.{extension}")
    }
}

fn exports_dir() -> PathBuf {
    glib::user_data_dir().join("forge").join("exports")
}

fn export_stamp() -> String {
    glib::DateTime::now_local()
        .ok()
        .and_then(|now| now.format("%Y%m%d-%H%M%S").ok())
        .map(|formatted| formatted.to_string())
        .unwrap_or_else(|| format!("pid{}", std::process::id()))
}

/// Assemble the session document out of per-block sections. Split from the
/// `TermView` accessor because the document shape is worth pinning in a test
/// and the accessor needs a live widget tree.
fn session_markdown_document(sections: &[String]) -> String {
    let mut md = String::new();

    md.push_str("# Terminal Session Export\n\n");
    md.push_str(&format!("Total blocks: {}\n\n", sections.len()));
    md.push_str("---\n\n");

    for (index, section) in sections.iter().enumerate() {
        md.push_str(&format!("## Block #{}\n\n", index + 1));
        md.push_str(section);
        md.push_str("\n---\n\n");
    }

    md
}

/// Write one export next to the block history, with the same owner-only
/// permissions: an export holds whatever the session's commands printed.
/// `create_new` in a loop is also what stops two exports taken in the same
/// second from silently overwriting each other.
fn write_session_export(
    directory: &Path,
    stamp: &str,
    extension: &str,
    contents: &str,
) -> io::Result<PathBuf> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(0o700);
    builder.create(directory)?;

    for attempt in 0..100u32 {
        let path = directory.join(export_file_name(stamp, extension, attempt));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(contents.as_bytes())?;
                file.flush()?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "too many session exports share this timestamp",
    ))
}

impl TermView {
    /// Export precisely these retained record IDs. The frozen selection is
    /// checked before any file is created; serialization is bounded too.
    pub(super) fn export_record_selection_to_file(
        &self,
        selected: &HashSet<u64>,
        format: SessionExportFormat,
    ) -> io::Result<PathBuf> {
        let contents = selected_export_document(&self.render_backend.records(), selected, format)?;
        write_session_export(
            &exports_dir(),
            &format!("selected-{}", export_stamp()),
            format.extension(),
            &contents,
        )
    }

    /// Export a block by ID to JSON format
    pub fn export_block_json(&self, block_id: u64) -> Option<String> {
        let records = self.render_backend.records();
        records
            .iter()
            .find(|record| record.id() == block_id)
            .map(record_json)
    }

    /// Export a block by ID to Markdown format
    pub fn export_block_markdown(&self, block_id: u64) -> Option<String> {
        let records = self.render_backend.records();
        records
            .iter()
            .find(|record| record.id() == block_id)
            .map(record_markdown)
    }

    /// Export all blocks in the session as JSON
    pub fn export_session_json(&self) -> String {
        let records = self.render_backend.records();
        records_json(&records)
    }

    /// Export all blocks in the session as Markdown
    pub fn export_session_markdown(&self) -> String {
        let records = self.render_backend.records();
        let sections: Vec<String> = records.iter().map(record_markdown).collect();
        session_markdown_document(&sections)
    }

    /// Write this session's blocks to a timestamped file under the forge data
    /// directory and report where it landed. The path is the caller's only way
    /// to tell the user where the export went, so it is returned rather than
    /// logged here.
    ///
    pub(crate) fn export_session_to_file(
        &self,
        format: SessionExportFormat,
    ) -> io::Result<PathBuf> {
        let contents = match format {
            SessionExportFormat::Markdown => self.export_session_markdown(),
            SessionExportFormat::Json => self.export_session_json(),
        };
        write_session_export(
            &exports_dir(),
            &export_stamp(),
            format.extension(),
            &contents,
        )
    }

    /// Copy a block's content to clipboard (prompt + cmd + output).
    pub fn copy_block_by_id(&self, block_id: u64) {
        let finished = self.finished_blocks.borrow();
        if let Some(block) = finished.iter().find(|b| b.id == block_id) {
            let prompt_text = block.prompt_text.clone();
            let cmd_text = block.cmd_text.clone();

            let full_text =
                block.with_stripped_output(|output| format!("{prompt_text}\n{cmd_text}\n{output}"));
            let clipboard = self.active_vte.clipboard();
            clipboard.set_text(&full_text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        export_file_name, metadata_record_markdown, record_json, record_markdown, records_json,
        session_markdown_document, write_session_export, BackendRecordRef, BackendRecords,
        CompletedCommandRecord, SessionExportFormat, ZoneOutputSnapshot,
    };
    use crate::block_view::UnifiedZoneStore;
    use std::cell::RefCell;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn selected_record(id: u64, command: &str) -> CompletedCommandRecord {
        CompletedCommandRecord {
            id,
            cmd: command.into(),
            exit_code: Some(0),
            start_time_ms: Some(100),
            end_time_ms: Some(200),
            duration_ms: Some(100),
            cwd: Some("/project".into()),
            is_background: false,
            completion_provenance: super::super::CompletionProvenance::ShellReported,
            command_source: super::super::CommandTextSource::ShellReported,
            start_mark_seen: true,
        }
    }

    #[test]
    fn selected_export_uses_exact_ids_in_terminal_order_and_rejects_retirement() {
        let mut store = UnifiedZoneStore::new();
        store.records.extend([
            selected_record(90, "older"),
            selected_record(30, "not selected"),
            selected_record(2, "newer"),
        ]);
        store.insert_snapshot(
            90,
            ZoneOutputSnapshot {
                plain: "old\n".into(),
                truncated: false,
            },
        );
        store.insert_snapshot(
            2,
            ZoneOutputSnapshot {
                plain: "tail\n".into(),
                truncated: true,
            },
        );
        let store = RefCell::new(store);
        let selected = std::collections::HashSet::from([2, 90]);
        let document = super::selected_export_document(
            &BackendRecords::Metadata(store.borrow()),
            &selected,
            SessionExportFormat::Json,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&document).unwrap();
        assert_eq!(value["scope"], "selected_records");
        let records = value["records"].as_array().unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["id"], 90);
        assert_eq!(records[1]["id"], 2);
        assert_eq!(records[1]["output_truncated"], true);
        assert!(!document.contains("not selected"));
        store.borrow_mut().records.pop_front();
        assert!(super::selected_export_document(
            &BackendRecords::Metadata(store.borrow()),
            &selected,
            SessionExportFormat::Json,
        )
        .unwrap_err()
        .to_string()
        .contains("no longer retained"));
    }

    #[test]
    fn selected_export_distinguishes_missing_from_empty_output_and_untrusted_timing() {
        let mut store = UnifiedZoneStore::new();
        let mut unknown = selected_record(1, "unknown status");
        unknown.exit_code = None;
        unknown.completion_provenance = super::super::CompletionProvenance::BoundaryInferred;
        store
            .records
            .extend([unknown, selected_record(2, "empty output")]);
        store.insert_snapshot(
            2,
            ZoneOutputSnapshot {
                plain: String::new(),
                truncated: false,
            },
        );
        let store = RefCell::new(store);
        let document = super::selected_export_document(
            &BackendRecords::Metadata(store.borrow()),
            &std::collections::HashSet::from([1, 2]),
            SessionExportFormat::Json,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&document).unwrap();
        let records = value["records"].as_array().unwrap();
        assert_eq!(records[0]["output_available"], false);
        assert!(records[0].get("output").is_none());
        assert!(records[0].get("output_truncated").is_none());
        assert_eq!(records[0]["exit_code"], serde_json::Value::Null);
        assert_eq!(records[0]["duration_ms"], serde_json::Value::Null);
        assert_eq!(records[1]["output_available"], true);
        assert_eq!(records[1]["output"], "");
        assert_eq!(records[1]["output_truncated"], false);
    }

    #[test]
    fn selected_markdown_preserves_fences_and_explicit_partial_capture() {
        let mut store = UnifiedZoneStore::new();
        store.records.push_back(selected_record(7, "printf '```'"));
        store.insert_snapshot(
            7,
            ZoneOutputSnapshot {
                plain: "```\nretained tail\n".into(),
                truncated: true,
            },
        );
        let store = RefCell::new(store);
        let document = super::selected_export_document(
            &BackendRecords::Metadata(store.borrow()),
            &std::collections::HashSet::from([7]),
            SessionExportFormat::Markdown,
        )
        .unwrap();
        assert!(document.contains("## Record #7"));
        assert!(document.contains("````sh\nprintf '```'\n````"));
        assert!(document.contains("````\n```\nretained tail\n\n````"));
        assert!(document.contains("partial retained text"));
    }

    #[test]
    fn selected_exports_reject_empty_selection_and_bound_encoded_bytes() {
        // The largest permitted selection remains exportable; adding one
        // retained record must reject the entire selection in both formats.
        let mut boundary_store = UnifiedZoneStore::new();
        boundary_store.records.extend(
            (1..=super::MAX_SELECTED_RECORDS as u64 + 1)
                .map(|id| selected_record(id, "small output")),
        );
        let boundary_store = RefCell::new(boundary_store);
        let allowed = (1..=super::MAX_SELECTED_RECORDS as u64).collect();
        let too_many = (1..=super::MAX_SELECTED_RECORDS as u64 + 1).collect();
        for format in [SessionExportFormat::Json, SessionExportFormat::Markdown] {
            assert!(super::selected_export_document(
                &BackendRecords::Metadata(boundary_store.borrow()),
                &allowed,
                format,
            )
            .is_ok());
            assert!(super::selected_export_document(
                &BackendRecords::Metadata(boundary_store.borrow()),
                &too_many,
                format,
            )
            .unwrap_err()
            .to_string()
            .contains("between 1 and 64"));
        }

        let mut store = UnifiedZoneStore::new();
        store
            .records
            .push_back(selected_record(1, "control output"));
        // JSON expands every retained NUL to six bytes. The serialized-byte
        // ceiling must reject it even though raw capture fits its own budget.
        store.insert_snapshot(
            1,
            ZoneOutputSnapshot {
                plain: "\0".repeat(super::MAX_SELECTED_EXPORT_BYTES / 6 + 1),
                truncated: false,
            },
        );
        let store = RefCell::new(store);
        assert!(super::selected_export_document(
            &BackendRecords::Metadata(store.borrow()),
            &std::collections::HashSet::new(),
            SessionExportFormat::Json,
        )
        .is_err());
        assert!(super::selected_export_document(
            &BackendRecords::Metadata(store.borrow()),
            &std::collections::HashSet::from([1]),
            SessionExportFormat::Json,
        )
        .unwrap_err()
        .to_string()
        .contains("8 MiB"));
    }

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "forge-export-{name}-{}-{unique}",
                std::process::id()
            ));
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn export_file_names_disambiguate_same_second_collisions() {
        assert_eq!(
            export_file_name(
                "20260725-101112",
                SessionExportFormat::Markdown.extension(),
                0
            ),
            "session-20260725-101112.md"
        );
        assert_eq!(
            export_file_name("20260725-101112", SessionExportFormat::Json.extension(), 2),
            "session-20260725-101112-2.json"
        );
    }

    #[test]
    fn session_document_numbers_every_block_and_states_the_total() {
        let document = session_markdown_document(&[
            "**Command:**\n```bash\nls\n```\n".to_string(),
            "**Command:**\n```bash\npwd\n```\n".to_string(),
        ]);

        assert_eq!(
            document,
            "# Terminal Session Export\n\nTotal blocks: 2\n\n---\n\n\
             ## Block #1\n\n**Command:**\n```bash\nls\n```\n\n---\n\n\
             ## Block #2\n\n**Command:**\n```bash\npwd\n```\n\n---\n\n"
        );
        assert_eq!(
            session_markdown_document(&[]),
            "# Terminal Session Export\n\nTotal blocks: 0\n\n---\n\n"
        );
    }

    #[test]
    fn unified_metadata_json_preserves_schema_without_fabricating_output() {
        let mut store = UnifiedZoneStore::new();
        store.records.extend([
            CompletedCommandRecord {
                id: 41,
                cmd: "cargo check --locked".to_string(),
                exit_code: Some(7),
                start_time_ms: Some(1_700_000_000_000),
                end_time_ms: Some(1_700_000_001_234),
                duration_ms: Some(1_234),
                cwd: Some("/work/forge".to_string()),
                is_background: false,
                completion_provenance: super::super::CompletionProvenance::ShellReported,
                command_source: super::super::CommandTextSource::Screen,
                start_mark_seen: true,
            },
            CompletedCommandRecord {
                id: 42,
                cmd: "mystery-command".to_string(),
                exit_code: None,
                start_time_ms: Some(1_700_000_002_000),
                end_time_ms: Some(1_700_000_002_250),
                duration_ms: Some(250),
                cwd: None,
                is_background: false,
                completion_provenance: super::super::CompletionProvenance::BoundaryInferred,
                command_source: super::super::CommandTextSource::Screen,
                start_mark_seen: true,
            },
            CompletedCommandRecord {
                id: 43,
                cmd: String::new(),
                exit_code: None,
                start_time_ms: None,
                end_time_ms: Some(1_700_000_003_000),
                duration_ms: None,
                cwd: Some("/work/forge".to_string()),
                is_background: true,
                completion_provenance: super::super::CompletionProvenance::Unknown,
                command_source: super::super::CommandTextSource::Screen,
                start_mark_seen: false,
            },
        ]);
        let records = RefCell::new(store);
        let records = BackendRecords::Metadata(records.borrow());
        let value: serde_json::Value =
            serde_json::from_str(&records_json(&records)).expect("valid metadata export JSON");

        assert_eq!(
            value,
            serde_json::json!([
                {
                    "id": 41,
                    "cmd": "cargo check --locked",
                    "exit_code": 7,
                    "start_time_ms": 1_700_000_000_000u64,
                    "end_time_ms": 1_700_000_001_234u64,
                    "duration_ms": 1_234,
                    "cwd": "/work/forge",
                    "is_background": false,
                    "completion_provenance": "shell_reported",
                    "lifecycle_health": "healthy",
                    "output_available": false
                },
                {
                    "id": 42,
                    "cmd": "mystery-command",
                    "exit_code": null,
                    "start_time_ms": null,
                    "end_time_ms": null,
                    "duration_ms": null,
                    "cwd": null,
                    "is_background": false,
                    "completion_provenance": "boundary_inferred",
                    "lifecycle_health": "degraded",
                    "output_available": false
                },
                {
                    "id": 43,
                    "cmd": "",
                    "exit_code": null,
                    "start_time_ms": null,
                    "end_time_ms": 1_700_000_003_000u64,
                    "duration_ms": null,
                    "cwd": "/work/forge",
                    "is_background": true,
                    "output_available": false
                }
            ])
        );
        for record in value.as_array().expect("session JSON is an array") {
            let object = record.as_object().expect("each record is an object");
            assert_eq!(
                object.get("output_available"),
                Some(&serde_json::json!(false))
            );
            assert!(
                !object.contains_key("output"),
                "metadata export must omit output rather than forge an empty string"
            );
        }
    }

    #[test]
    fn unified_metadata_markdown_is_honest_for_unknown_status_and_background_output() {
        let unknown = CompletedCommandRecord {
            id: 71,
            cmd: "mystery-command".to_string(),
            exit_code: None,
            start_time_ms: Some(10),
            end_time_ms: Some(1_244),
            duration_ms: Some(1_234),
            cwd: Some("/work/forge".to_string()),
            is_background: false,
            completion_provenance: super::super::CompletionProvenance::BoundaryInferred,
            command_source: super::super::CommandTextSource::Screen,
            start_mark_seen: true,
        };
        let unknown_json: serde_json::Value =
            serde_json::from_str(&record_json(BackendRecordRef::Metadata {
                record: &unknown,
                snapshot: None,
            }))
            .expect("valid single-record JSON");
        assert_eq!(unknown_json["exit_code"], serde_json::Value::Null);
        assert_eq!(unknown_json["output_available"], false);
        assert!(unknown_json.get("output").is_none());
        assert!(unknown_json.get("output_truncated").is_none());
        assert_eq!(
            record_markdown(BackendRecordRef::Metadata {
                record: &unknown,
                snapshot: None,
            }),
            "## Command Record\n\n**Command:**\n```bash\nmystery-command\n```\n\n\
             **Output:** unavailable (retained on the live Unified terminal surface only)\n\n\
             **Exit Code:** unknown (the shell reported none)\n\n\
             **Lifecycle:** degraded (boundary_inferred)\n\n"
        );

        let background = CompletedCommandRecord {
            id: 72,
            cmd: String::new(),
            exit_code: None,
            start_time_ms: None,
            end_time_ms: Some(2_000),
            duration_ms: None,
            cwd: Some("/work/forge".to_string()),
            is_background: true,
            completion_provenance: super::super::CompletionProvenance::Unknown,
            command_source: super::super::CommandTextSource::Screen,
            start_mark_seen: false,
        };
        let markdown = record_markdown(BackendRecordRef::Metadata {
            record: &background,
            snapshot: None,
        });
        assert_eq!(
            markdown,
            "## Background Output\n\n\
             **Output:** unavailable (retained on the live Unified terminal surface only)\n\n"
        );
        assert!(!markdown.contains("**Command:**"));
        assert!(!markdown.contains("**Exit Code:**"));
        assert!(
            !markdown.contains("```"),
            "no empty output fence is fabricated"
        );
    }

    #[test]
    fn unified_metadata_with_snapshot_exports_output_in_json_and_markdown() {
        let record = CompletedCommandRecord {
            id: 81,
            cmd: "cargo test".to_string(),
            exit_code: Some(0),
            start_time_ms: None,
            end_time_ms: None,
            duration_ms: Some(2_500),
            cwd: None,
            is_background: false,
            completion_provenance: super::super::CompletionProvenance::ShellReported,
            command_source: super::super::CommandTextSource::Screen,
            start_mark_seen: true,
        };
        let snapshot = ZoneOutputSnapshot {
            plain: "running 3 tests\ntest result: ok".to_string(),
            truncated: false,
        };

        let json: serde_json::Value =
            serde_json::from_str(&record_json(BackendRecordRef::Metadata {
                record: &record,
                snapshot: Some(&snapshot),
            }))
            .expect("valid single-record JSON");
        assert_eq!(json["output"], "running 3 tests\ntest result: ok");
        assert_eq!(json["output_truncated"], false);
        assert_eq!(json["output_available"], true);

        assert_eq!(
            record_markdown(BackendRecordRef::Metadata {
                record: &record,
                snapshot: Some(&snapshot),
            }),
            "## Command Record\n\n**Command:**\n```bash\ncargo test\n```\n\n\
             **Output:**\n```\nrunning 3 tests\ntest result: ok\n```\n\n\
             **Exit Code:** 0\n\n\
             **Lifecycle:** healthy (shell_reported)\n\n\
             **Duration:** 2.500s\n\n"
        );
    }

    #[test]
    fn truncated_snapshot_markdown_carries_the_tail_bound_note() {
        let record = CompletedCommandRecord {
            id: 82,
            cmd: "cat big.log".to_string(),
            exit_code: Some(0),
            start_time_ms: None,
            end_time_ms: None,
            duration_ms: None,
            cwd: None,
            is_background: false,
            completion_provenance: super::super::CompletionProvenance::ShellReported,
            command_source: super::super::CommandTextSource::Screen,
            start_mark_seen: true,
        };
        let snapshot = ZoneOutputSnapshot {
            plain: "…last lines".to_string(),
            truncated: true,
        };

        let markdown = record_markdown(BackendRecordRef::Metadata {
            record: &record,
            snapshot: Some(&snapshot),
        });
        assert!(markdown
            .contains("**Output:** (truncated to the last 64 KiB)\n```\n…last lines\n```\n\n"));

        let json: serde_json::Value =
            serde_json::from_str(&record_json(BackendRecordRef::Metadata {
                record: &record,
                snapshot: Some(&snapshot),
            }))
            .expect("valid single-record JSON");
        assert_eq!(json["output_truncated"], true);
    }

    /// Budget eviction removes only snapshot bytes; the surviving record must
    /// export exactly like one that never had output retained.
    #[test]
    fn budget_evicted_snapshot_exports_as_unavailable_again() {
        let mut store = UnifiedZoneStore::new();
        store.records.push_back(CompletedCommandRecord {
            id: 90,
            cmd: "make".to_string(),
            exit_code: Some(0),
            start_time_ms: None,
            end_time_ms: None,
            duration_ms: None,
            cwd: None,
            is_background: false,
            completion_provenance: super::super::CompletionProvenance::ShellReported,
            command_source: super::super::CommandTextSource::Screen,
            start_mark_seen: true,
        });
        store.insert_snapshot(
            90,
            ZoneOutputSnapshot {
                plain: "compiling".to_string(),
                truncated: false,
            },
        );
        store.enforce_snapshot_budget(0);

        let store = RefCell::new(store);
        let records = BackendRecords::Metadata(store.borrow());
        let value: serde_json::Value =
            serde_json::from_str(&records_json(&records)).expect("valid metadata export JSON");
        let object = value[0].as_object().expect("one exported record");
        assert_eq!(object.get("cmd"), Some(&serde_json::json!("make")));
        assert_eq!(
            object.get("output_available"),
            Some(&serde_json::json!(false))
        );
        assert!(!object.contains_key("output"));
        assert!(!object.contains_key("output_truncated"));

        let markdown = store
            .borrow()
            .records
            .front()
            .map(|record| {
                record_markdown(BackendRecordRef::Metadata {
                    record,
                    snapshot: store.borrow().snapshot(90),
                })
            })
            .expect("one record");
        assert!(markdown.contains("**Output:** unavailable"));
    }

    #[test]
    fn metadata_markdown_fences_untrusted_command_and_output_backticks() {
        let record = CompletedCommandRecord {
            id: 91,
            cmd: "printf '```'".to_string(),
            exit_code: Some(0),
            start_time_ms: None,
            end_time_ms: None,
            duration_ms: None,
            cwd: None,
            is_background: false,
            completion_provenance: super::super::CompletionProvenance::ShellReported,
            command_source: super::super::CommandTextSource::Screen,
            start_mark_seen: true,
        };
        let snapshot = ZoneOutputSnapshot {
            plain: "```\n## not a heading".to_string(),
            truncated: false,
        };
        let markdown = metadata_record_markdown(&record, Some(&snapshot));
        assert!(markdown.contains("**Command:**\n````bash\nprintf '```'\n````\n\n"));
        assert!(markdown.contains("**Output:**\n````\n```\n## not a heading\n````\n\n"));
    }

    /// An export is command output on disk, so it must not be world-readable,
    /// and a second export in the same second must not overwrite the first.
    #[test]
    fn session_exports_are_owner_only_and_never_overwrite_each_other() {
        let dir = TestDir::new("collision");

        let first = write_session_export(dir.path(), "20260725-101112", "md", "first").unwrap();
        let second = write_session_export(dir.path(), "20260725-101112", "md", "second").unwrap();

        assert_eq!(
            first.file_name().unwrap(),
            "session-20260725-101112.md",
            "the first export of a second keeps the plain name"
        );
        assert_eq!(second.file_name().unwrap(), "session-20260725-101112-1.md");
        assert_eq!(fs::read_to_string(&first).unwrap(), "first");
        assert_eq!(fs::read_to_string(&second).unwrap(), "second");
        assert_eq!(
            fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700,
            "the exports directory itself must not be readable by others"
        );
    }
}
