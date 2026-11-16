use serde::Serialize;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SharedEntry {
    name: String,
    directory: bool,
    data: Option<Vec<u8>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserImage {
    name: String,
    path: String,
    read_only: bool,
}

fn is_cd_image(name: &str) -> bool {
    let name = name.to_lowercase();
    name.ends_with(".iso") || name.ends_with(".toast")
}

fn is_disk_image(name: &str) -> bool {
    let name = name.to_lowercase();
    name.ends_with(".img") || name.ends_with(".dsk") || name.ends_with(".hda")
}

fn normalize_relative_path(relative: &str) -> Result<PathBuf, String> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err("shared paths must be relative".to_string());
    }

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => normalized.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "shared path escapes the macintosh.js directory: {relative}"
                ));
            }
        }
    }

    if normalized.as_os_str().is_empty() {
        return Err("shared paths must name a file".to_string());
    }
    Ok(normalized)
}

fn resolve_shared_path(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = normalize_relative_path(relative)?;
    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("could not resolve {}: {error}", root.display()))?;
    let target = canonical_root.join(&relative);

    // Resolve the existing part of the target. This rejects traversal through
    // directory symlinks and existing file symlinks that point outside the
    // shared folder, while still allowing files directly inside the root.
    let mut existing = target.as_path();
    loop {
        match fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                existing = existing
                    .parent()
                    .ok_or_else(|| format!("could not resolve {}", target.display()))?;
            }
            Err(error) => {
                return Err(format!("could not resolve {}: {error}", existing.display()));
            }
        }
    }
    let canonical_existing = existing
        .canonicalize()
        .map_err(|error| format!("could not resolve {}: {error}", existing.display()))?;
    if !canonical_existing.starts_with(&canonical_root) {
        return Err(format!(
            "shared path escapes the macintosh.js directory: {}",
            relative.display()
        ));
    }
    Ok(target)
}

#[tauri::command]
pub fn list_user_images(directory: String) -> Result<Vec<UserImage>, String> {
    let directory = PathBuf::from(directory);
    let mut images = Vec::new();
    let Ok(entries) = fs::read_dir(directory) else {
        return Ok(images);
    };
    let mut index = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_disk_image(name) && !is_cd_image(name) {
            continue;
        }
        let safe = format!(
            "user_image_{index}_{}",
            name.replace(
                |character: char| !(character.is_alphanumeric()
                    || character == '.'
                    || character == ' '),
                ""
            )
        );
        index += 1;
        images.push(UserImage {
            name: safe,
            path: path.display().to_string(),
            read_only: is_cd_image(name),
        });
    }
    Ok(images)
}

#[tauri::command]
pub fn read_shared_tree(host_base: String) -> Result<Vec<SharedEntry>, String> {
    let root = PathBuf::from(host_base);
    let mut entries = Vec::new();
    read_tree(&root, &mut entries)?;
    Ok(entries)
}

fn read_tree(root: &Path, entries: &mut Vec<SharedEntry>) -> Result<(), String> {
    let Ok(directory) = fs::read_dir(root) else {
        return Ok(());
    };
    for entry in directory.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || is_disk_image(&name) || is_cd_image(&name) {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            entries.push(SharedEntry {
                name: path
                    .strip_prefix(root)
                    .map(|relative| relative.display().to_string())
                    .map_err(|error| format!("could not resolve {}: {error}", path.display()))?,
                directory: true,
                data: None,
            });
            read_tree(&path, entries)?;
        } else if path.is_file() {
            let data = fs::read(&path)
                .map_err(|error| format!("could not read {}: {error}", path.display()))?;
            entries.push(SharedEntry {
                name: path
                    .strip_prefix(root)
                    .map(|relative| relative.display().to_string())
                    .map_err(|error| format!("could not resolve {}: {error}", path.display()))?,
                directory: false,
                data: Some(data),
            });
        }
    }
    Ok(())
}

#[tauri::command]
pub fn write_shared_file(host_base: String, path: String, data: Vec<u8>) -> Result<(), String> {
    let root = PathBuf::from(host_base);
    fs::create_dir_all(&root)
        .map_err(|error| format!("could not create {}: {error}", root.display()))?;

    // Validate before creating anything so a traversal path cannot make
    // directories outside the shared folder as a side effect.
    let target = resolve_shared_path(&root, &path)?;
    let parent = target
        .parent()
        .ok_or_else(|| format!("could not resolve {}", target.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    let target = resolve_shared_path(&root, &path)?;

    fs::write(&target, data)
        .map_err(|error| format!("could not write {}: {error}", target.display()))
}

#[cfg(test)]
mod tests {
    use super::resolve_shared_path;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    static NEXT_TEMP_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    fn temporary_directory() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before UNIX epoch")
            .as_nanos();
        let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "macintosh-js-extfs-{}-{unique}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("create temporary directory");
        directory
    }

    #[test]
    fn accepts_files_directly_inside_root() {
        let root = temporary_directory();
        let canonical_root = root.canonicalize().expect("canonicalize root");
        let resolved = resolve_shared_path(&root, "Read Me.txt").expect("resolve top-level file");
        assert_eq!(resolved, canonical_root.join("Read Me.txt"));
        fs::remove_dir_all(root).expect("remove temporary directory");
    }

    #[test]
    fn accepts_nested_paths_that_do_not_exist_yet() {
        let root = temporary_directory();
        let canonical_root = root.canonicalize().expect("canonicalize root");
        let resolved =
            resolve_shared_path(&root, "Projects/1997/demo.txt").expect("resolve nested file");
        assert_eq!(resolved, canonical_root.join("Projects/1997/demo.txt"));
        fs::remove_dir_all(root).expect("remove temporary directory");
    }

    #[test]
    fn rejects_path_traversal() {
        let root = temporary_directory();
        assert!(resolve_shared_path(&root, "../outside.txt").is_err());
        assert!(resolve_shared_path(&root, "nested/../../outside.txt").is_err());
        fs::remove_dir_all(root).expect("remove temporary directory");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_escape_root() {
        use std::os::unix::fs::symlink;

        let root = temporary_directory();
        let outside = temporary_directory();
        symlink(&outside, root.join("escape")).expect("create escaping symlink");
        assert!(resolve_shared_path(&root, "escape/file.txt").is_err());
        fs::remove_dir_all(root).expect("remove temporary directory");
        fs::remove_dir_all(outside).expect("remove temporary directory");
    }
}
