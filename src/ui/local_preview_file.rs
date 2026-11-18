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
    if directory.is_empty() || directory.len() > 4096 || directory.chars().any(char::is_control) {
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
    Ok(base.join(candidate))
}

#[derive(Debug)]
pub enum PreviewContent {
    Text(String),
    Image(super::local_preview_image::RgbaPreview),
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
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > limit {
        return Err(io::Error::other("Preview requires a bounded regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other("Preview is too large"));
    }
    Ok(bytes)
}

pub fn read_source(path: &Path) -> io::Result<String> {
    let bytes = read_bounded(path, MAX_BYTES)?;
    if bytes.contains(&0) {
        return Err(io::Error::other("Preview contains binary data"));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| io::Error::other("Preview requires UTF-8 source text"))?;
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
            let before = source.metadata()?;
            if !before.is_file() || before.len() > limit {
                return Err(io::Error::other("Expected a bounded regular file"));
            }
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            // This is our still-owned descriptor, not a second lookup of the input path.
            let file = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(nix::libc::O_NONBLOCK | nix::libc::O_CLOEXEC)
                .open(format!("/proc/self/fd/{}", source.as_raw_fd()))?;
            let after = file.metadata()?;
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
    use super::*;
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
