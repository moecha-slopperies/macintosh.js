use std::{
    fs,
    path::{Path, PathBuf},
};
use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};

#[cfg(feature = "ppc")]
pub const APP_FOLDER_NAME: &str = "macintosh.js-ppc";
#[cfg(not(feature = "ppc"))]
pub const APP_FOLDER_NAME: &str = "macintosh.js";

#[cfg(feature = "ppc")]
pub const APP_TITLE: &str = "macintosh.js PPC";
#[cfg(not(feature = "ppc"))]
pub const APP_TITLE: &str = "macintosh.js";

#[cfg(feature = "ppc")]
pub const CORE_RESOURCE_DIR: &str = "sheepshaver";
#[cfg(not(feature = "ppc"))]
pub const CORE_RESOURCE_DIR: &str = "basilisk";

#[cfg(feature = "ppc")]
pub const DISK_ASSEMBLE_COMMAND: &str = "npm run disk:assemble:ppc";
#[cfg(not(feature = "ppc"))]
pub const DISK_ASSEMBLE_COMMAND: &str = "npm run disk:assemble";

pub fn home_dir() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| "could not determine the home directory".to_string())
}

pub fn shared_dir() -> Result<PathBuf, String> {
    Ok(home_dir()?.join(APP_FOLDER_NAME))
}

pub fn user_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let config_dir = app
        .path()
        .app_config_dir()
        .map_err(|error| format!("could not determine the app config directory: {error}"))?;
    let parent = config_dir
        .parent()
        .ok_or_else(|| format!("could not determine the parent of {}", config_dir.display()))?;
    Ok(parent.join(APP_FOLDER_NAME))
}

pub fn dev_marker(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(user_data_dir(app)?.join("developer"))
}

pub fn is_dev_mode(app: &AppHandle) -> Result<bool, String> {
    if tauri::is_dev() || std::env::var_os("MACINTOSH_JS_DEV").is_some() {
        return Ok(true);
    }
    Ok(dev_marker(app)?.exists())
}

pub fn set_dev_mode(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let marker = dev_marker(app)?;
    if enabled {
        if let Some(parent) = marker.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
        }
        fs::write(&marker, "So you're a developer, huh? Neat! Welcome aboard!")
            .map_err(|error| format!("could not create {}: {error}", marker.display()))?;
    } else if marker.exists() {
        fs::remove_file(&marker)
            .map_err(|error| format!("could not remove {}: {error}", marker.display()))?;
    }
    Ok(())
}

pub fn ensure_shared_dir() -> Result<PathBuf, String> {
    let directory = shared_dir()?;
    fs::create_dir_all(&directory)
        .map_err(|error| format!("could not create {}: {error}", directory.display()))?;
    Ok(directory)
}

pub fn reveal_shared_dir(_app: &AppHandle) -> Result<(), String> {
    let directory = ensure_shared_dir()?;
    tauri_plugin_opener::reveal_item_in_dir(&directory)
        .map_err(|error| format!("could not reveal {}: {error}", directory.display()))
}

pub fn open_license(app: &AppHandle) -> Result<(), String> {
    let path = app
        .path()
        .resource_dir()
        .map_err(|error| format!("could not determine resource directory: {error}"))?
        .join(CORE_RESOURCE_DIR)
        .join("LICENSE.txt");
    tauri_plugin_opener::open_path(path, None::<&str>)
        .map_err(|error| format!("could not open the license: {error}"))
}

pub fn copy_initial_disk(resources: &Path, target: &Path) -> Result<(), String> {
    if target.exists() {
        return Ok(());
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let source = resources.join(CORE_RESOURCE_DIR).join("disk");
    if source.exists() {
        return fs::copy(&source, target)
            .map(|_| ())
            .map_err(|error| format!("could not prepare the VM disk: {error}"));
    }

    if cfg!(debug_assertions) {
        Ok(())
    } else {
        Err(format!(
            "bundled disk image is missing: {}. Run `{}` before packaging.",
            source.display(),
            DISK_ASSEMBLE_COMMAND
        ))
    }
}

pub fn confirm(app: &AppHandle, title: &str, message: &str, yes: &str, no: &str) -> bool {
    app.dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Warning)
        .buttons(MessageDialogButtons::OkCancelCustom(
            yes.to_string(),
            no.to_string(),
        ))
        .blocking_show()
}
