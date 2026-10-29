use super::{GraphicError, MAX_SIZE};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Validate the object behind the open handle, including symlink targets.
fn opened_path(file: &File) -> Result<PathBuf, GraphicError> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
            .map_err(|_| GraphicError::InvalidData)
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;
        let mut buffer = [0u8; libc::PATH_MAX as usize];
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETPATH, buffer.as_mut_ptr()) }
            < 0
        {
            return Err(GraphicError::InvalidData);
        }
        let len = buffer
            .iter()
            .position(|&b| b == 0)
            .ok_or(GraphicError::InvalidData)?;
        Ok(PathBuf::from(std::ffi::OsStr::from_bytes(&buffer[..len])))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;
        let mut buffer = vec![0u16; 32768];
        let len = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        } as usize;
        if len == 0 || len >= buffer.len() {
            return Err(GraphicError::InvalidData);
        }
        Ok(PathBuf::from(std::ffi::OsString::from_wide(&buffer[..len])))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = file;
        Err(GraphicError::UnsupportedMedium)
    }
}

fn validate_path(
    path: &Path,
    temporary: bool,
    roots: &[PathBuf],
) -> Result<(), GraphicError> {
    #[cfg(unix)]
    if path.starts_with("/proc")
        || path.starts_with("/sys")
        || (path.starts_with("/dev") && !path.starts_with("/dev/shm"))
    {
        return Err(GraphicError::InvalidData);
    }
    if temporary
        && (!roots.iter().any(|root| path.starts_with(root))
            || !path.file_name().is_some_and(|name| {
                name.to_string_lossy().contains("tty-graphics-protocol")
            }))
    {
        return Err(GraphicError::InvalidData);
    }
    Ok(())
}

pub(super) fn read(
    payload: &[u8],
    offset: u32,
    size: u32,
    temporary: bool,
) -> Result<Vec<u8>, GraphicError> {
    let path =
        Path::new(std::str::from_utf8(payload).map_err(|_| GraphicError::InvalidData)?);
    #[cfg(windows)]
    {
        // Permit drive-absolute filesystem paths only. Device namespaces,
        // UNC paths, alternate streams and reserved DOS names must not open.
        let text = path.to_string_lossy();
        let bytes = text.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || !matches!(bytes[2], b'\\' | b'/')
            || text[2..].contains(':')
        {
            return Err(GraphicError::InvalidData);
        }
        for part in text[3..].split(['/', '\\']) {
            let name = part
                .split('.')
                .next()
                .unwrap_or("")
                .trim_end_matches(' ')
                .to_ascii_uppercase();
            if matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (name.len() == 4
                    && (name.starts_with("COM") || name.starts_with("LPT"))
                    && matches!(name.as_bytes()[3], b'1'..=b'9'))
            {
                return Err(GraphicError::InvalidData);
            }
        }
    }
    if size as usize > MAX_SIZE {
        return Err(GraphicError::TooLarge);
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A FIFO must not block the parser before its type can be checked.
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY);
    }
    let mut file = options.open(path).map_err(|_| GraphicError::InvalidData)?;
    if !file
        .metadata()
        .map_err(|_| GraphicError::InvalidData)?
        .is_file()
    {
        return Err(GraphicError::InvalidData);
    }
    let canonical = opened_path(&file)?;
    let roots: Vec<_> = [
        std::env::temp_dir(),
        PathBuf::from("/tmp"),
        PathBuf::from("/dev/shm"),
    ]
    .into_iter()
    .filter_map(|root| root.canonicalize().ok())
    .collect();
    validate_path(&canonical, temporary, &roots)?;
    // Apply O even when S is omitted. S is an exact byte count.
    let result = (|| {
        file.seek(SeekFrom::Start(offset as u64))
            .map_err(|_| GraphicError::InvalidData)?;
        let mut data = Vec::new();
        if size != 0 {
            data.resize(size as usize, 0);
            file.read_exact(&mut data)
                .map_err(|_| GraphicError::InvalidData)?;
        } else {
            file.by_ref()
                .take(MAX_SIZE as u64 + 1)
                .read_to_end(&mut data)
                .map_err(|_| GraphicError::InvalidData)?;
            if data.len() > MAX_SIZE {
                return Err(GraphicError::TooLarge);
            }
        }
        Ok(data)
    })();
    drop(file);
    if temporary {
        if let Err(error) = std::fs::remove_file(&canonical) {
            tracing::warn!(?error, path = ?canonical, "Failed to delete Kitty temporary image");
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_files_require_a_root_and_matching_basename() {
        let roots = [PathBuf::from("/tmp/allowed")];
        assert!(validate_path(
            Path::new("/tmp/allowed/tty-graphics-protocol-image"),
            true,
            &roots
        )
        .is_ok());
        for path in [
            "/tmp/allowed-other/tty-graphics-protocol-image",
            "/tmp/outside/tty-graphics-protocol-image",
            "/tmp/allowed/image",
            "/tmp/allowed/tty-graphics-protocol-dir/image",
        ] {
            assert!(
                validate_path(Path::new(path), true, &roots).is_err(),
                "{path}"
            );
        }
    }

    #[test]
    fn file_offsets_apply_without_explicit_sizes() {
        let path = std::env::temp_dir()
            .join(format!("rio-kitty-file-offset-{}", std::process::id()));
        std::fs::write(&path, [9, 1, 2, 3, 4]).unwrap();
        let data = read(path.to_str().unwrap().as_bytes(), 1, 0, false).unwrap();
        assert_eq!(data, [1, 2, 3, 4]);
        assert!(read(path.to_str().unwrap().as_bytes(), 2, 4, false).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn symlinks_are_checked_against_the_opened_target() {
        use std::os::unix::fs::symlink;
        let path = std::env::temp_dir().join(format!(
            "tty-graphics-protocol-symlink-{}",
            std::process::id()
        ));
        for target in ["/proc/self/environ", "/etc/hostname"] {
            symlink(target, &path).unwrap();
            assert!(read(path.to_str().unwrap().as_bytes(), 0, 4, true).is_err());
            assert!(path.symlink_metadata().is_ok());
            std::fs::remove_file(&path).unwrap();
        }
    }

    #[test]
    #[cfg(unix)]
    fn fifo_transport_is_rejected_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let path =
            std::env::temp_dir().join(format!("rio-kitty-fifo-{}", std::process::id()));
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let result = read(path.to_str().unwrap().as_bytes(), 0, 0, false);
        std::fs::remove_file(path).unwrap();
        assert!(result.is_err());
    }
}
