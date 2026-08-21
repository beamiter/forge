//! export — extracted from block_view (mechanical split, no logic changes)
//!
//! Serializes finished blocks to JSON / Markdown for the user-facing export
//! actions, plus a clipboard-copy helper for the per-block right-click menu.
//! Reads the completed-record snapshot owned by the active render backend; no
//! terminal state mutation other than the explicit clipboard helper.

use gtk4::glib;
use gtk4::prelude::*;
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
            .then_some(record.completion_provenance.as_str()),
        lifecycle_health: (!record.is_background).then_some(record.lifecycle_health().as_str()),
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
            record.lifecycle_health().as_str(),
            record.completion_provenance.as_str(),
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
