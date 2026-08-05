//! export — extracted from block_view (mechanical split, no logic changes)
//!
//! Serializes finished blocks to JSON / Markdown for the user-facing export
//! actions, plus a clipboard-copy helper for the per-block right-click menu.
//! Reads only the in-memory `block_data` and `finished_blocks` snapshots; no
//! VTE state mutation.

use gtk4::glib;
use gtk4::prelude::*;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use super::TermView;

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
        let blocks = self.block_data.borrow();
        blocks
            .iter()
            .find(|b| b.id == block_id)
            .map(|b| b.to_json())
    }

    /// Export a block by ID to Markdown format
    pub fn export_block_markdown(&self, block_id: u64) -> Option<String> {
        let blocks = self.block_data.borrow();
        blocks
            .iter()
            .find(|b| b.id == block_id)
            .map(|b| b.to_markdown())
    }

    /// Export all blocks in the session as JSON
    pub fn export_session_json(&self) -> String {
        let blocks = self.block_data.borrow();
        serde_json::to_string_pretty(&*blocks).unwrap_or_else(|_| "[]".to_string())
    }

    /// Export all blocks in the session as Markdown
    pub fn export_session_markdown(&self) -> String {
        let blocks = self.block_data.borrow();
        let sections: Vec<String> = blocks.iter().map(|block| block.to_markdown()).collect();
        session_markdown_document(&sections)
    }

    /// Write this session's blocks to a timestamped file under the forge data
    /// directory and report where it landed. The path is the caller's only way
    /// to tell the user where the export went, so it is returned rather than
    /// logged here.
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
        export_file_name, session_markdown_document, write_session_export, SessionExportFormat,
    };
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
