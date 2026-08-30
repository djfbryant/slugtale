use serde::{de::DeserializeOwned, Serialize};

// A loader must not quarantine a valid file that an app save replaced after the read.
static JSON_FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn load_or_default<T>(path: &std::path::Path) -> T
where
    T: DeserializeOwned + Default,
{
    let _guard = lock();
    let Ok(bytes) = std::fs::read(path) else {
        return T::default();
    };

    match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            quarantine(path, &bytes);
            T::default()
        }
    }
}

pub(crate) fn save<T>(path: &std::path::Path, value: &T) -> std::io::Result<()>
where
    T: Serialize,
{
    let _guard = lock();
    let json = serde_json::to_string_pretty(value)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("data.json");
    let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));

    std::fs::write(&temp_path, json)?;
    match std::fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp_path);
            Err(error)
        }
    }
}

pub(crate) fn delete_with_quarantines(path: &std::path::Path) -> std::io::Result<()> {
    let _guard = lock();
    let mut first_error = remove_file_if_present(path).err();
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return first_error.map_or(Ok(()), Err);
    };
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return first_error.map_or(Ok(()), Err)
        }
        Err(error) => return Err(first_error.unwrap_or(error)),
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
                continue;
            }
        };
        let Some(candidate) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if is_quarantine_name(&candidate, file_name) {
            if let Err(error) = remove_file_if_present(&entry.path()) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }

    first_error.map_or(Ok(()), Err)
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    JSON_FILE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn remove_file_if_present(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn is_quarantine_name(candidate: &str, file_name: &str) -> bool {
    let Some(suffix) = candidate.strip_prefix(file_name) else {
        return false;
    };
    if suffix == ".corrupt" {
        return true;
    }
    let Some(number) = suffix
        .strip_prefix('.')
        .and_then(|suffix| suffix.strip_suffix(".corrupt"))
    else {
        return false;
    };

    number
        .bytes()
        .enumerate()
        .all(|(index, byte)| byte.is_ascii_digit() && (index > 0 || byte != b'0'))
        && !number.is_empty()
}

fn quarantine(path: &std::path::Path, bytes: &[u8]) {
    use std::io::Write;

    let file_name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("data.json"));

    for index in 0usize.. {
        let mut quarantine_name = file_name.to_os_string();
        if index > 0 {
            quarantine_name.push(format!(".{index}"));
        }
        quarantine_name.push(".corrupt");
        let quarantine_path = path.with_file_name(quarantine_name);

        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&quarantine_path)
        {
            Ok(mut file) => {
                let result = file.write_all(bytes).and_then(|()| file.flush());
                drop(file);
                if result.is_err() {
                    let _ = std::fs::remove_file(quarantine_path);
                }
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if std::fs::read(quarantine_path).is_ok_and(|existing| existing == bytes) {
                    return;
                }
            }
            Err(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Deserialize)]
    struct FileShape {
        value: u32,
    }

    #[test]
    fn valid_json_with_an_incompatible_shape_is_quarantined() {
        let path = unique_test_path("incompatible");
        let recovery = path.with_file_name(format!(
            "{}.corrupt",
            path.file_name().unwrap().to_string_lossy()
        ));
        std::fs::write(&path, br#"{"value":"wrong type"}"#).unwrap();

        let loaded = load_or_default::<FileShape>(&path);

        assert_eq!(loaded, FileShape::default());
        assert!(path.exists());
        assert_eq!(
            std::fs::read(&recovery).unwrap(),
            br#"{"value":"wrong type"}"#
        );
        std::fs::remove_file(path).ok();
        std::fs::remove_file(recovery).ok();
    }

    #[test]
    fn an_unreadable_path_is_left_alone() {
        let path = unique_test_path("unreadable");
        std::fs::create_dir(&path).unwrap();
        let recovery = path.with_file_name(format!(
            "{}.corrupt",
            path.file_name().unwrap().to_string_lossy()
        ));

        let loaded = load_or_default::<FileShape>(&path);

        assert_eq!(loaded, FileShape::default());
        assert!(path.is_dir());
        assert!(!recovery.exists());
        std::fs::remove_dir(path).ok();
    }

    #[test]
    fn quarantine_names_match_only_the_files_the_loader_creates() {
        assert!(is_quarantine_name("usage.json.corrupt", "usage.json"));
        assert!(is_quarantine_name("usage.json.12.corrupt", "usage.json"));
        assert!(!is_quarantine_name("usage.json.0.corrupt", "usage.json"));
        assert!(!is_quarantine_name("usage.json.01.corrupt", "usage.json"));
        assert!(!is_quarantine_name(
            "other-usage.json.corrupt",
            "usage.json"
        ));
        assert!(!is_quarantine_name(
            "usage.json.corrupt.notes",
            "usage.json"
        ));
    }

    #[test]
    fn quarantine_uses_the_read_snapshot_not_the_current_source() {
        let path = unique_test_path("replaced-source");
        let recovery = path.with_file_name(format!(
            "{}.corrupt",
            path.file_name().unwrap().to_string_lossy()
        ));
        let snapshot = b"original malformed bytes";
        let replacement = b"new editor bytes";
        std::fs::write(&path, replacement).unwrap();

        quarantine(&path, snapshot);

        assert_eq!(std::fs::read(&recovery).unwrap(), snapshot);
        assert_eq!(std::fs::read(&path).unwrap(), replacement);
        std::fs::remove_file(path).ok();
        std::fs::remove_file(recovery).ok();
    }

    #[test]
    fn loading_the_same_bad_bytes_twice_reuses_the_recovery() {
        let path = unique_test_path("repeated");
        let quarantine = path.with_file_name(format!(
            "{}.corrupt",
            path.file_name().unwrap().to_string_lossy()
        ));
        let next_quarantine = path.with_file_name(format!(
            "{}.1.corrupt",
            path.file_name().unwrap().to_string_lossy()
        ));
        let malformed = br#"{"value":"wrong type"}"#;
        std::fs::write(&path, malformed).unwrap();

        let first = load_or_default::<FileShape>(&path);
        let second = load_or_default::<FileShape>(&path);

        assert_eq!(first, FileShape::default());
        assert_eq!(second, FileShape::default());
        assert_eq!(std::fs::read(&path).unwrap(), malformed);
        assert_eq!(std::fs::read(&quarantine).unwrap(), malformed);
        assert!(!next_quarantine.exists());
        std::fs::remove_file(path).ok();
        std::fs::remove_file(quarantine).ok();
    }

    fn unique_test_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "slugtale-json-file-{name}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
