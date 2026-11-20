//! Bounded, nonexecuting local source preview. Caller must obtain local-file intent.
use std::io::{self, Read};
use std::path::Path;

pub const MAX_BYTES: u64 = 256 * 1024;

/// Resolve only the directory explicitly typed into the local confirmation UI.
/// Never consult terminal cwd, environment variables, home expansion or the OS cwd.
pub fn selected_local_path(candidate: &Path, directory: &str) -> io::Result<std::path::PathBuf> {
    use std::path::Component;
    let invalid = |p: &Path| {
        p.components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    };
    if candidate.as_os_str().is_empty() || invalid(candidate) {
        return Err(io::Error::other("Parent paths are not previewed"));
    }
    if candidate.is_absolute() {
        return Ok(candidate.to_path_buf());
    }
    if directory.is_empty()
        || directory.len() > 4096
        || directory
            .chars()
            .any(|c| c.is_control() || jterm_core::review_input::is_visual_spoofing_character(c))
    {
        return Err(io::Error::other(
            "Enter an absolute directory on this machine",
        ));
    }
    let base = Path::new(directory);
    if !base.is_absolute() || invalid(base) || candidate.starts_with("~") {
        return Err(io::Error::other(
            "Choose a local directory without parent or home expansion",
        ));
    }
    let target = base.join(candidate);
    if target.as_os_str().len() > 4096 {
        return Err(io::Error::other("Local path is too long"));
    }
    Ok(target)
}

#[derive(Debug)]
pub enum PreviewContent {
    Text(String),
    Image(super::local_preview_image::RgbaPreview),
}

pub struct LocatedPreviewContent {
    pub content: PreviewContent,
    pub path: std::path::PathBuf,
    pub location: Option<super::local_reference::SourceLocationCandidate>,
}

/// Pure disclosure; never probes either candidate before user confirmation.
pub fn confirmation_target(path: &Path) -> String {
    let exact = format!("Literal local file\n{}", path.display());
    match path
        .to_str()
        .and_then(super::local_reference::parse_candidate)
        .and_then(|candidate| candidate.source_location)
    {
        Some(location) => {
            let coordinates = match location.column {
                Some(column) => format!("line {}, column {column} (reported)", location.line),
                None => format!("line {}", location.line),
            };
            format!(
                "{exact}\n\nFallback when the literal file is missing\n{} ({coordinates})",
                location.path
            )
        }
        None => exact,
    }
}

pub fn read_reference(path: &Path) -> io::Result<LocatedPreviewContent> {
    read_reference_with(path, read_preview)
}

fn read_reference_with(
    path: &Path,
    mut read: impl FnMut(&Path) -> io::Result<PreviewContent>,
) -> io::Result<LocatedPreviewContent> {
    match read(path) {
        Ok(content) => Ok(LocatedPreviewContent {
            content,
            path: path.to_path_buf(),
            location: None,
        }),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let Some(location) = path
                .to_str()
                .and_then(super::local_reference::parse_candidate)
                .and_then(|candidate| candidate.source_location)
            else {
                return Err(error);
            };
            let target = std::path::PathBuf::from(&location.path);
            let content = read(&target)?;
            Ok(LocatedPreviewContent {
                content,
                path: target,
                location: Some(location),
            })
        }
        Err(error) => Err(error),
    }
}

pub fn read_preview(path: &Path) -> io::Result<PreviewContent> {
    let image = path
        .extension()
        .and_then(|x| x.to_str())
        .is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"));
    if image {
        let bytes = read_bounded(path, super::local_preview_image::MAX_IMAGE_BYTES)?;
        return super::local_preview_image::decode(&bytes).map(PreviewContent::Image);
    }
    read_source(path).map(PreviewContent::Text)
}

fn read_bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    if !path.is_absolute() {
        return Err(io::Error::other("Choose an absolute local path"));
    }
    let file = open_without_symlink_components(path, limit)?;
    let metadata = file.metadata().map_err(non_lookup_error)?;
    if !metadata.is_file() {
        return Err(io::Error::other("Only regular files can be previewed"));
    }
    if metadata.len() > limit {
        return Err(size_error(limit));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(non_lookup_error)?;
    if bytes.len() as u64 > limit {
        return Err(size_error(limit));
    }
    Ok(bytes)
}

fn size_error(limit: u64) -> io::Error {
    let message = if limit == MAX_BYTES {
        "Text preview exceeds the 256 KiB limit"
    } else {
        "PNG/JPEG preview exceeds the 4 MiB limit"
    };
    io::Error::other(message)
}

// Only original path-component lookup may report absence to a location fallback.
fn non_lookup_error(error: io::Error) -> io::Error {
    if error.kind() == io::ErrorKind::NotFound {
        io::Error::other("Opened local file became unavailable")
    } else {
        error
    }
}

pub fn read_source(path: &Path) -> io::Result<String> {
    let bytes = read_bounded(path, MAX_BYTES)?;
    if bytes.contains(&0) {
        return Err(io::Error::other("Preview contains binary data"));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| io::Error::other("Preview requires UTF-8 source text"))?;
    // Display CRLF as one line break without rewriting the source file.
    // Lone CR remains a visible escape; it must never act as cursor movement.
    let text = text.replace("\r\n", "\n");
    // Render controls explicitly, including bidi direction and invisible formatting.
    Ok(text.chars().flat_map(|c| {
        if (c.is_control() && c != '\n' && c != '\t')
            || matches!(c, '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}') {
            c.escape_unicode().collect::<Vec<_>>()
        } else { vec![c] }
    }).collect())
}

#[cfg(target_os = "linux")]
fn open_without_symlink_components(path: &Path, limit: u64) -> io::Result<std::fs::File> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    let components = path.components().collect::<Vec<_>>();
    if components.first() != Some(&Component::RootDir) || components.len() < 2 {
        return Err(io::Error::other("Expected an absolute regular file path"));
    }
    // Each open is relative to an owned directory FD; replacing path components
    // cannot redirect a later open through a newly inserted symlink.
    let root = CString::new("/").unwrap();
    let raw = unsafe {
        nix::libc::open(
            root.as_ptr(),
            nix::libc::O_RDONLY | nix::libc::O_DIRECTORY | nix::libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut directory = unsafe { OwnedFd::from_raw_fd(raw) };
    for (index, component) in components.iter().enumerate().skip(1) {
        let Component::Normal(name) = component else {
            return Err(io::Error::other(
                "Preview does not resolve parent components",
            ));
        };
        let name =
            CString::new(name.as_bytes()).map_err(|_| io::Error::other("Invalid local path"))?;
        let final_component = index + 1 == components.len();
        let flags = nix::libc::O_PATH
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | if final_component {
                0
            } else {
                nix::libc::O_DIRECTORY
            };
        let raw = unsafe { nix::libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let opened = unsafe { OwnedFd::from_raw_fd(raw) };
        if final_component {
            let source = std::fs::File::from(opened);
            let before = source.metadata().map_err(non_lookup_error)?;
            if !before.is_file() {
                return Err(io::Error::other("Only regular files can be previewed"));
            }
            if before.len() > limit {
                return Err(size_error(limit));
            }
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            // This is our still-owned descriptor, not a second lookup of the input path.
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
                .open(format!("/proc/self/fd/{}", source.as_raw_fd()))
                .map_err(non_lookup_error)?;
            let after = file.metadata().map_err(non_lookup_error)?;
            if !after.is_file() || before.dev() != after.dev() || before.ino() != after.ino() {
                return Err(io::Error::other("Local file identity changed"));
            }
            return Ok(file);
        }
        directory = opened;
    }
    Err(io::Error::other("Expected a regular file"))
}

#[cfg(not(target_os = "linux"))]
fn open_without_symlink_components(_: &Path, _: u64) -> io::Result<std::fs::File> {
    Err(io::Error::other(
        "Safe local preview is only supported on Linux",
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn post_lookup_absence_is_not_a_fallback_signal() {
        assert_eq!(
            non_lookup_error(io::Error::from(io::ErrorKind::NotFound)).kind(),
            io::ErrorKind::Other
        );
        assert_eq!(
            non_lookup_error(io::Error::from(io::ErrorKind::PermissionDenied)).kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[test]
    fn disclosure_names_both_candidates_without_io() {
        let note = confirmation_target(Path::new("/missing/project/App.tsx:12:3"));
        assert!(note.contains("Literal local file\n/missing/project/App.tsx:12:3"));
        assert!(note.contains(
            "Fallback when the literal file is missing\n/missing/project/App.tsx (line 12"
        ));
        assert_eq!(
            confirmation_target(Path::new("/missing/plain.md")),
            "Literal local file\n/missing/plain.md"
        );
    }

    #[test]
    fn location_fallback_only_follows_lookup_absence() {
        for kind in [
            io::ErrorKind::PermissionDenied,
            io::ErrorKind::InvalidData,
            io::ErrorKind::Other,
        ] {
            let mut calls = 0;
            let result = read_reference_with(Path::new("/local/App.tsx:12:3"), |_| {
                calls += 1;
                Err(io::Error::from(kind))
            });
            assert!(result.is_err());
            assert_eq!(calls, 1);
        }
    }

    #[test]
    fn literal_colon_file_precedes_location_and_unsafe_files_never_fall_back() {
        let dir =
            std::env::temp_dir().join(format!("forge-preview-location-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let base = dir.join("App.tsx");
        let exact = dir.join("App.tsx:12:3");
        std::fs::write(&base, "BASE").unwrap();
        std::fs::write(&exact, "LITERAL").unwrap();
        let result = read_reference(&exact).unwrap();
        assert_eq!(result.path, exact);
        assert!(result.location.is_none());
        assert!(matches!(result.content, PreviewContent::Text(s) if s == "LITERAL"));
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&exact, std::fs::Permissions::from_mode(0o000)).unwrap();
        let denied = read_reference(&exact);
        std::fs::set_permissions(&exact, std::fs::Permissions::from_mode(0o600)).unwrap();
        // Root may bypass mode bits; the injected error test covers that policy too.
        if unsafe { libc::geteuid() } != 0 {
            assert_eq!(
                denied.err().unwrap().kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        std::fs::remove_file(&exact).unwrap();
        let result = read_reference(&exact).unwrap();
        assert_eq!(result.path, base);
        assert_eq!(result.location.unwrap().line.get(), 12);
        assert!(matches!(result.content, PreviewContent::Text(s) if s == "BASE"));
        std::os::unix::fs::symlink(&base, &exact).unwrap();
        assert!(read_reference(&exact).is_err());
        std::fs::remove_file(&exact).unwrap();
        std::fs::write(&exact, [0xff]).unwrap();
        assert!(read_reference(&exact).is_err());
        std::fs::remove_file(&exact).unwrap();
        std::fs::create_dir(&exact).unwrap();
        assert!(read_reference(&exact).is_err());
        std::fs::remove_dir(&exact).unwrap();
        std::fs::remove_file(&base).unwrap();
        assert_eq!(
            read_reference(&exact).err().unwrap().kind(),
            io::ErrorKind::NotFound
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn crlf_display_preserves_line_numbers_and_original_bytes() {
        let dir = std::env::temp_dir().join(format!("forge-preview-crlf-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("source.md");
        let original = "first\r\nsecond\nthird\r\r\nfourth\rtail\t\u{1b}\u{202e}\r\n";
        std::fs::write(&path, original.as_bytes()).unwrap();
        let displayed = read_source(&path).unwrap();
        assert_eq!(
            displayed,
            "first\nsecond\nthird\\u{d}\nfourth\\u{d}tail\t\\u{1b}\\u{202e}\n"
        );
        assert_eq!(
            displayed.matches('\n').count(),
            original.matches('\n').count()
        );
        assert_eq!(displayed.lines().nth(1), Some("second"));
        assert_eq!(std::fs::read(&path).unwrap(), original.as_bytes());
        std::fs::remove_dir_all(dir).unwrap();
    }

    use super::*;
    #[test]
    #[cfg(target_os = "linux")]
    fn delimited_paths_never_redirect_between_space_distinct_files() {
        let dir = std::env::temp_dir().join(format!("forge-path-identity-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let exact = dir.join("report- notes.md");
        let other = dir.join("report-notes.md");
        std::fs::write(&exact, "EXACT").unwrap();
        std::fs::write(&other, "OTHER").unwrap();
        let single = format!("({})", exact.display());
        let references = crate::delimited_reference::detect(&[(&single, false)]);
        assert_eq!(references.len(), 1);
        assert!(!references[0].blocked);
        assert_eq!(
            read_source(Path::new(&references[0].target)).unwrap(),
            "EXACT"
        );
        assert_eq!(read_source(&other).unwrap(), "OTHER");
        // A hard newline and a full-width-looking row are equally ambiguous.
        // Neither may activate the trimmed OTHER path or mutate clipboard text.
        for full_width in [false, true] {
            for (head, tail) in [("report- ", "notes.md)"), ("report-", " notes.md)")] {
                let first = format!("({}/{head}", dir.display());
                let references =
                    crate::delimited_reference::detect(&[(&first, full_width), (tail, false)]);
                assert_eq!(references.len(), 1);
                assert!(references[0].blocked);
                assert!(references[0].target.contains('\n'));
                assert_ne!(references[0].target, other.to_str().unwrap());
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn image_reads_share_the_regular_file_and_symlink_guard() {
        use image::ImageEncoder;
        let dir = std::env::temp_dir().join(format!("forge-image-preview-{}", std::process::id()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("picture.PNG");
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&[1, 2, 3, 255], 1, 1, image::ExtendedColorType::Rgba8)
            .unwrap();
        std::fs::write(&path, png).unwrap();
        assert!(matches!(
            read_preview(&path).unwrap(),
            PreviewContent::Image(_)
        ));
        let link = dir.join("link.png");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_preview(&link).is_err());
        let parent = dir.join("parent");
        std::os::unix::fs::symlink(&dir, &parent).unwrap();
        assert!(read_preview(&parent.join("picture.PNG")).is_err());
        assert!(read_preview(Path::new("/dev/null.png")).is_err());
        std::fs::write(&path, "<script>not an image</script>").unwrap();
        assert!(read_preview(&path).is_err());
        std::fs::File::create(&path)
            .unwrap()
            .set_len(super::super::local_preview_image::MAX_IMAGE_BYTES + 1)
            .unwrap();
        assert!(read_preview(&path).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn relative_paths_require_an_explicit_local_directory() {
        assert_eq!(
            selected_local_path(Path::new("src/App.tsx"), "/chosen/project").unwrap(),
            Path::new("/chosen/project/src/App.tsx")
        );
        assert_eq!(
            selected_local_path(Path::new("./README.md"), "/chosen/project").unwrap(),
            Path::new("/chosen/project/./README.md")
        );
        assert_eq!(
            selected_local_path(Path::new("/tmp/source.md"), "").unwrap(),
            Path::new("/tmp/source.md")
        );
        for (path, base) in [
            ("src/a.md", ""),
            ("src/a.md", "relative"),
            ("../a.md", "/tmp"),
            ("src/../a.md", "/tmp"),
            ("a.md", "/tmp/../other"),
            ("~/a.md", "/tmp"),
            ("a.md", "/tmp/\nspoof"),
        ] {
            assert!(
                selected_local_path(Path::new(path), base).is_err(),
                "{path} {base}"
            );
        }
        assert!(selected_local_path(Path::new("a.md"), &format!("/{}", "a".repeat(4096))).is_err());
    }
    #[test]
    #[cfg(target_os = "linux")]
    fn local_source_is_bounded_and_never_interpreted() {
        let dir = std::env::temp_dir().join(format!("forge-source-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("source.tsx");
        std::fs::write(&path, "<script>throw 1</script>\n\u{202e}").unwrap();
        assert_eq!(
            read_source(&path).unwrap(),
            "<script>throw 1</script>\n\\u{202e}"
        );
        std::fs::write(&path, [0]).unwrap();
        assert!(read_source(&path).is_err());
        std::fs::write(&path, vec![b'a'; MAX_BYTES as usize + 1]).unwrap();
        assert!(read_source(&path).is_err());
        assert!(read_source(&dir).is_err());
        assert!(read_source(Path::new("/dev/null")).is_err());
        let fifo = dir.join("pipe.md");
        use std::os::unix::ffi::OsStrExt;
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { nix::libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        assert!(read_source(&fifo).is_err());
        assert!(read_source(Path::new("relative.md")).is_err());
        #[cfg(unix)]
        {
            let link = dir.join("link.md");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read_source(&link).is_err());
            let parent_link = dir.join("parent");
            std::os::unix::fs::symlink(&dir, &parent_link).unwrap();
            assert!(read_source(&parent_link.join("source.tsx")).is_err());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
