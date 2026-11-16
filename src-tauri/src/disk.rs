use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    path::PathBuf,
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

pub fn prepare_boot_disk(app: &tauri::AppHandle) -> Result<DiskSpec, String> {
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
