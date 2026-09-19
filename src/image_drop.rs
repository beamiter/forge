//! Safe local-path drag payloads for terminal prompts.
//!
//! Desktop drag-and-drop supplies decoded local paths. We turn those paths into
//! ordinary review-first terminal input: no newline, no implicit submission,
//! and no control or bidi-spoofing characters. Any existing file or directory
//! is accepted — dropping a source file, a log or a folder is how people hand
//! a path to claude, codex or kimi — and the pane sends the payload as a paste
//! (bracketed when the program asked for it), as GNOME Terminal and kitty do.

use std::fmt;
use std::path::PathBuf;

const MAX_DROPPED_PATHS: usize = 16;
const MAX_DROP_PAYLOAD_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DropError(&'static str);

impl fmt::Display for DropError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

pub(crate) fn dropped_paths_payload(paths: &[PathBuf]) -> Result<String, DropError> {
    if paths.is_empty() {
        return Err(DropError("the drop contained no local files"));
    }
    if paths.len() > MAX_DROPPED_PATHS {
        return Err(DropError("too many files were dropped at once"));
    }

    let mut quoted = Vec::with_capacity(paths.len());
    let mut bytes = 0usize;
    for path in paths {
        if !path.exists() {
            return Err(DropError(
                "only existing local files and folders can be dropped",
            ));
        }
        let text = path
            .to_str()
            .ok_or(DropError("the dropped path is not valid UTF-8"))?;
        if text.chars().any(char::is_control)
            || jterm_core::review_input::contains_visual_spoofing(text)
        {
            return Err(DropError(
                "the dropped path contains hidden or control text",
            ));
        }

        let encoded = if text
            .chars()
            .all(|character| character.is_alphanumeric() || "._-/~".contains(character))
        {
            text.to_string()
        } else {
            jterm_core::process::shell_single_quote(text)
        };
        bytes = bytes
            .checked_add(encoded.len() + 1)
            .ok_or(DropError("the dropped paths are too long"))?;
        if bytes > MAX_DROP_PAYLOAD_BYTES {
            return Err(DropError("the dropped paths are too long"));
        }
        quoted.push(encoded);
    }

    let mut payload = quoted.join(" ");
    // Match desktop terminal drag behavior: leave the caret ready for more
    // prompt text without ever adding Enter.
    payload.push(' ');
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temporary_path(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "jterm-path-drop-{}-{nonce}-{name}",
            std::process::id()
        ))
    }

    fn temporary_file(name: &str) -> PathBuf {
        let path = temporary_path(name);
        File::create(&path).expect("temporary file");
        path
    }

    #[test]
    fn image_paths_are_shell_quoted_and_never_submitted() {
        let image = temporary_file("screen shot's.PNG");
        let payload = dropped_paths_payload(std::slice::from_ref(&image)).expect("valid image");
        assert_eq!(
            payload,
            format!(
                "{} ",
                jterm_core::process::shell_single_quote(image.to_str().unwrap())
            )
        );
        assert!(!payload.contains('\n'));
        assert!(!payload.contains('\r'));
        std::fs::remove_file(image).expect("cleanup");
    }

    #[test]
    fn any_existing_file_or_folder_can_be_dropped() {
        let notes = temporary_file("notes.txt");
        let folder = temporary_path("a folder");
        std::fs::create_dir(&folder).expect("temporary folder");
        let payload = dropped_paths_payload(&[notes.clone(), folder.clone()]).expect("valid drop");
        assert_eq!(
            payload,
            format!(
                "{} {} ",
                notes.to_str().unwrap(),
                jterm_core::process::shell_single_quote(folder.to_str().unwrap())
            )
        );
        std::fs::remove_file(notes).expect("cleanup");
        std::fs::remove_dir(folder).expect("cleanup");
    }

    #[test]
    fn missing_control_and_spoofed_paths_are_rejected() {
        assert_eq!(
            dropped_paths_payload(&[temporary_path("gone.txt")]).unwrap_err(),
            DropError("only existing local files and folders can be dropped")
        );
        for name in ["line\nbreak.txt", "bidi\u{202e}txt.exe"] {
            let path = temporary_file(name);
            assert_eq!(
                dropped_paths_payload(std::slice::from_ref(&path)).unwrap_err(),
                DropError("the dropped path contains hidden or control text"),
                "{name:?}"
            );
            std::fs::remove_file(path).expect("cleanup");
        }
        assert_eq!(
            dropped_paths_payload(&[]).unwrap_err(),
            DropError("the drop contained no local files")
        );
    }
}
