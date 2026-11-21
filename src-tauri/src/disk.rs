use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ffi::OsString,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
    sync::{Mutex, MutexGuard},
};
use tauri::{Manager, State};

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskSpec {
    name: String,
    path: PathBuf,
    read_only: bool,
}

struct OpenDisk {
    file: File,
    path: PathBuf,
    read_only: bool,
    size: u64,
}

pub struct DiskState {
    disks: Mutex<HashMap<String, DiskSpec>>,
    handles: Mutex<HashMap<u32, OpenDisk>>,
    next_handle: AtomicU32,
}

pub struct BootDiskOverride {
    path: Option<PathBuf>,
}

impl BootDiskOverride {
    pub fn from_env_and_args() -> Result<Self, String> {
        Self::from_args(
            std::env::var_os("MACINTOSH_JS_DISK").map(PathBuf::from),
            std::env::args_os().skip(1),
        )
    }

    fn from_args<I, S>(env_path: Option<PathBuf>, args: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let mut path = env_path;
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            let text = arg.into().to_string_lossy().into_owned();
            if text == "--disk" || text == "-d" {
                let value = args
                    .next()
                    .ok_or_else(|| "--disk requires a path".to_string())?;
                path = Some(PathBuf::from(value.into()));
            } else if let Some(value) = text.strip_prefix("--disk=") {
                if value.is_empty() {
                    return Err("--disk requires a path".to_string());
                }
                path = Some(PathBuf::from(value));
            }
        }
        Ok(Self { path })
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }
}

impl DiskState {
    pub fn new() -> Self {
        Self {
            disks: Mutex::new(HashMap::new()),
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU32::new(1),
        }
    }

    fn disks(&self) -> MutexGuard<'_, HashMap<String, DiskSpec>> {
        self.disks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn handles(&self) -> MutexGuard<'_, HashMap<u32, OpenDisk>> {
        self.handles
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[tauri::command]
pub fn register_disks(disks: Vec<DiskSpec>, state: State<'_, DiskState>) -> Result<(), String> {
    let mut registered = state.disks();
    registered.retain(|name, _| name == "disk");
    for disk in disks {
        registered.insert(disk.name.clone(), disk);
    }
    Ok(())
}

pub fn prepare_boot_disk(
    app: &tauri::AppHandle,
    boot: &BootDiskOverride,
) -> Result<DiskSpec, String> {
    if let Some(path) = boot.path() {
        let path = resolve_boot_override(path)?;
        println!("disk: using boot image {}", path.display());
        return Ok(DiskSpec {
            name: "disk".to_string(),
            path,
            read_only: false,
        });
    }

    let user_data = crate::platform::user_data_dir(app)?;
    let target = user_data.join("disk");
    let resources = app
        .path()
        .resource_dir()
        .map_err(|error| format!("could not locate resources: {error}"))?;
    crate::platform::copy_initial_disk(&resources, &target)?;
    Ok(DiskSpec {
        name: "disk".to_string(),
        path: target,
        read_only: false,
    })
}

fn resolve_boot_override(path: &Path) -> Result<PathBuf, String> {
    let resolved = std::fs::canonicalize(path)
        .map_err(|error| format!("could not resolve disk image {}: {error}", path.display()))?;
    let metadata = std::fs::metadata(&resolved)
        .map_err(|error| format!("could not stat disk image {}: {error}", resolved.display()))?;
    if !metadata.is_file() {
        return Err(format!("disk image is not a file: {}", resolved.display()));
    }
    Ok(resolved)
}

#[tauri::command]
pub fn disk_open(name: String, state: State<'_, DiskState>) -> Result<i32, String> {
    let spec = state
        .disks()
        .get(&name)
        .cloned()
        .ok_or_else(|| format!("unknown disk image: {name}"))?;

    let file = OpenOptions::new()
        .read(true)
        .write(!spec.read_only)
        .open(&spec.path)
        .map_err(|error| format!("could not open {}: {error}", spec.path.display()))?;
    let size = file
        .metadata()
        .map_err(|error| format!("could not stat {}: {error}", spec.path.display()))?
        .len();

    let mut handles = state.handles();
    let handle = state.next_handle.fetch_add(1, Ordering::Relaxed);
    handles.insert(
        handle,
        OpenDisk {
            file,
            path: spec.path,
            read_only: spec.read_only,
            size,
        },
    );
    Ok(handle as i32)
}

#[tauri::command]
pub fn disk_close(handle: u32, state: State<'_, DiskState>) {
    state.handles().remove(&handle);
}

#[tauri::command]
pub fn disk_size(handle: u32, state: State<'_, DiskState>) -> u64 {
    state.handles().get(&handle).map_or(0, |disk| disk.size)
}

#[tauri::command]
pub fn disk_read(
    handle: u32,
    offset: u64,
    length: u32,
    state: State<'_, DiskState>,
) -> Result<tauri::ipc::Response, String> {
    #[cfg(unix)]
    use std::os::unix::fs::FileExt as FileExtUnix;

    let handles = state.handles();
    let disk = handles
        .get(&handle)
        .ok_or_else(|| format!("unknown disk handle: {handle}"))?;
    let mut buffer = vec![0; length as usize];
    #[cfg(unix)]
    let read = disk
        .file
        .read_at(&mut buffer, offset)
        .map_err(|error| format!("could not read {}: {error}", disk.path.display()))?;
    #[cfg(windows)]
    let read = {
        use std::os::windows::fs::FileExt as FileExtWindows;
        disk.file
            .seek_read(&mut buffer, offset)
            .map_err(|error| format!("could not read {}: {error}", disk.path.display()))?
    };
    buffer.truncate(read);
    Ok(tauri::ipc::Response::new(buffer))
}

#[tauri::command]
pub fn disk_write(
    request: tauri::ipc::Request<'_>,
    state: State<'_, DiskState>,
) -> Result<u32, String> {
    let tauri::ipc::InvokeBody::Raw(data) = request.body() else {
        return Err("disk_write expects a raw byte payload".to_string());
    };
    let header = |name: &str| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    let handle: u32 = header("x-disk-handle")
        .ok_or_else(|| "missing x-disk-handle header".to_string())?
        .parse()
        .map_err(|error| format!("invalid x-disk-handle: {error}"))?;
    let offset: u64 = header("x-disk-offset")
        .ok_or_else(|| "missing x-disk-offset header".to_string())?
        .parse()
        .map_err(|error| format!("invalid x-disk-offset: {error}"))?;

    let handles = state.handles();
    let disk = handles
        .get(&handle)
        .ok_or_else(|| format!("unknown disk handle: {handle}"))?;
    if disk.read_only {
        return Ok(0);
    }
    #[cfg(unix)]
    let written = {
        use std::os::unix::fs::FileExt as FileExtUnix;
        disk.file
            .write_at(data.as_slice(), offset)
            .map_err(|error| format!("could not write {}: {error}", disk.path.display()))?
    };
    #[cfg(windows)]
    let written = {
        use std::os::windows::fs::FileExt as FileExtWindows;
        disk.file
            .seek_write(data.as_slice(), offset)
            .map_err(|error| format!("could not write {}: {error}", disk.path.display()))?
    };
    Ok(written as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_disk_override_arguments() {
        let boot = BootDiskOverride::from_args(None, ["--disk", "/tmp/boot.dsk"]).unwrap();
        assert_eq!(boot.path(), Some(Path::new("/tmp/boot.dsk")));

        let boot = BootDiskOverride::from_args(
            Some(PathBuf::from("/tmp/env.dsk")),
            ["--disk=/tmp/arg.dsk"],
        )
        .unwrap();
        assert_eq!(boot.path(), Some(Path::new("/tmp/arg.dsk")));

        let boot = BootDiskOverride::from_args(
            Some(PathBuf::from("/tmp/env.dsk")),
            std::iter::empty::<&str>(),
        )
        .unwrap();
        assert_eq!(boot.path(), Some(Path::new("/tmp/env.dsk")));

        assert!(BootDiskOverride::from_args(None, ["--disk"]).is_err());
    }

    #[test]
    fn rejects_missing_disk_override() {
        let error = resolve_boot_override(Path::new("/this/path/does/not/exist.dsk")).unwrap_err();
        assert!(error.contains("could not resolve"));
    }

    #[test]
    fn resolves_existing_disk_override() {
        let path = std::env::temp_dir().join(format!(
            "macintosh-js-disk-override-{}.dsk",
            std::process::id()
        ));
        std::fs::write(&path, b"not really a disk").unwrap();

        let resolved = resolve_boot_override(&path).unwrap();
        assert!(resolved.is_absolute());
        assert_eq!(std::fs::read(&resolved).unwrap(), b"not really a disk");

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn rejects_directory_as_disk_override() {
        let error = resolve_boot_override(&std::env::temp_dir()).unwrap_err();
        assert!(error.contains("not a file"));
    }
}
