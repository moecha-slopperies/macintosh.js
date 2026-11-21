mod disk;
mod ethernet;
mod extfs;
mod local_server;
mod platform;

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_dialog::{DialogExt, MessageDialogKind};
use url::Url;

use disk::{BootDiskOverride, DiskState};
use ethernet::EthernetState;

static QUIT_CONFIRMED: AtomicBool = AtomicBool::new(false);

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInfo {
    version: String,
    user_data_path: String,
    shared_path: String,
    is_dev_mode: bool,
}

#[tauri::command]
async fn app_info(app: AppHandle) -> Result<AppInfo, String> {
    Ok(AppInfo {
        version: app.package_info().version.to_string(),
        user_data_path: platform::user_data_dir(&app)?.display().to_string(),
        shared_path: platform::shared_dir()?.display().to_string(),
        is_dev_mode: platform::is_dev_mode(&app)?,
    })
}

#[tauri::command]
fn get_shared_path() -> Result<String, String> {
    Ok(platform::shared_dir()?.display().to_string())
}

#[tauri::command]
fn boot_disk_info(
    app: AppHandle,
    boot: tauri::State<'_, BootDiskOverride>,
) -> Result<disk::DiskSpec, String> {
    disk::prepare_boot_disk(&app, boot.inner())
}

#[tauri::command]
fn is_dev_mode(app: AppHandle) -> Result<bool, String> {
    platform::is_dev_mode(&app)
}

#[tauri::command]
fn set_dev_mode(app: AppHandle, enabled: bool) -> Result<(), String> {
    platform::set_dev_mode(&app, enabled)
}

#[tauri::command]
fn reveal_shared_dir(app: AppHandle) -> Result<(), String> {
    platform::reveal_shared_dir(&app)
}

#[tauri::command]
fn open_license(app: AppHandle) -> Result<(), String> {
    platform::open_license(&app)
}

#[tauri::command]
async fn show_error(app: AppHandle, title: String, message: String) -> Result<(), String> {
    app.dialog()
        .message(message)
        .title(title)
        .kind(MessageDialogKind::Error)
        .blocking_show();
    Ok(())
}

#[tauri::command]
fn toggle_devtools(window: tauri::WebviewWindow) {
    #[cfg(desktop)]
    {
        if window.is_devtools_open() {
            window.close_devtools();
        } else {
            window.open_devtools();
        }
    }
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

fn copy_bundle_resources(app: &AppHandle) -> Result<(), String> {
    let user_data = platform::user_data_dir(app)?;
    fs::create_dir_all(&user_data)
        .map_err(|error| format!("could not create {}: {error}", user_data.display()))?;

    if app.state::<BootDiskOverride>().path().is_none() {
        let resources = app
            .path()
            .resource_dir()
            .map_err(|error| format!("could not locate resources: {error}"))?;
        platform::copy_initial_disk(&resources, &user_data.join("disk"))?;
    }

    Ok(())
}

#[derive(Clone)]
struct AppOrigin {
    base: Url,
    is_dev: bool,
}

impl AppOrigin {
    fn new(app: &AppHandle) -> Result<Self, String> {
        if tauri::is_dev() {
            let base = app.config().build.dev_url.clone().unwrap_or_else(|| {
                Url::parse("http://localhost:1420/").expect("valid fallback dev URL")
            });
            return Ok(Self {
                base: with_trailing_slash(base),
                is_dev: true,
            });
        }

        Ok(Self {
            base: local_server::start(app)?.base_url,
            is_dev: false,
        })
    }

    fn url(&self, page: &str) -> Result<Url, String> {
        self.base
            .join(page)
            .map_err(|error| format!("could not build the app URL for {page}: {error}"))
    }

    fn webview_url(&self, page: &str) -> Result<WebviewUrl, String> {
        if self.is_dev {
            Ok(WebviewUrl::App(PathBuf::from(page)))
        } else {
            Ok(WebviewUrl::External(self.url(page)?))
        }
    }

    fn is_local(&self, url: &Url) -> bool {
        self.base.make_relative(url).is_some()
    }
}

fn with_trailing_slash(mut url: Url) -> Url {
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    url.set_query(None);
    url.set_fragment(None);
    url
}

fn child_page(url: &Url) -> Option<&'static str> {
    let file_name = url.path().rsplit('/').next()?;
    ["help", "credits"]
        .into_iter()
        .find(|page| file_name == *page || file_name == format!("{page}.html"))
}

fn handle_navigation(origin: &AppOrigin, url: &Url) -> bool {
    if origin.is_local(url) {
        return true;
    }
    if matches!(url.scheme(), "http" | "https") {
        if let Err(error) = tauri_plugin_opener::open_url(url.as_str(), None::<&str>) {
            eprintln!("could not open external URL {url}: {error}");
        }
    }
    false
}

fn open_child_window(app: &AppHandle, origin: &AppOrigin, url: &Url) -> Result<(), String> {
    if !origin.is_local(url) {
        return Err(format!("refusing to open external window URL: {url}"));
    }
    let page = child_page(url).ok_or_else(|| format!("unsupported window URL: {url}"))?;
    let label = format!("child-{page}");
    if let Some(existing) = app.get_webview_window(&label) {
        let _ = existing.show();
        let _ = existing.set_focus();
        return Ok(());
    }
    let app = app.clone();
    let origin = origin.clone();
    let page = page.to_string();
    std::thread::spawn(move || {
        let Ok(webview_url) = origin.webview_url(&format!("{page}.html")) else {
            return;
        };
        let window = WebviewWindowBuilder::new(&app, label, webview_url)
            .title(if page == "help" { "Help" } else { "Credits" })
            .inner_size(350.0, 630.0)
            .resizable(true)
            .on_navigation({
                let origin = origin.clone();
                move |url| handle_navigation(&origin, url)
            })
            .build();
        if let Ok(window) = window {
            if platform::is_dev_mode(&app).unwrap_or(false) {
                window.open_devtools();
            }
        }
    });
    Ok(())
}

fn setup_window(app: &AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let origin = AppOrigin::new(app)?;
    let window = WebviewWindowBuilder::new(app, "main", origin.webview_url("index.html")?)
        .title("macintosh.js")
        .inner_size(800.0, 655.0)
        .transparent(true)
        .decorations(false)
        .on_navigation({
            let origin = origin.clone();
            move |url| handle_navigation(&origin, url)
        })
        .on_new_window({
            let app = app.clone();
            let origin = origin.clone();
            move |url, _features| {
                if origin.is_local(&url) && child_page(&url).is_some() {
                    let _ = open_child_window(&app, &origin, &url);
                } else if matches!(url.scheme(), "http" | "https") {
                    if let Err(error) = tauri_plugin_opener::open_url(url.as_str(), None::<&str>) {
                        eprintln!("could not open external URL {url}: {error}");
                    }
                }
                tauri::webview::NewWindowResponse::Deny
            }
        })
        .build()?;
    if platform::is_dev_mode(app).unwrap_or(false) {
        window.open_devtools();
    }
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let boot_disk = disk::BootDiskOverride::from_env_and_args().unwrap_or_else(|error| {
        eprintln!("macintosh.js: {error}");
        std::process::exit(2);
    });

    tauri::Builder::default()
        .manage(DiskState::new())
        .manage(EthernetState::new())
        .manage(boot_disk)
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            app_info,
            get_shared_path,
            boot_disk_info,
            is_dev_mode,
            set_dev_mode,
            reveal_shared_dir,
            open_license,
            show_error,
            toggle_devtools,
            quit,
            disk::register_disks,
            disk::disk_open,
            disk::disk_close,
            disk::disk_size,
            disk::disk_read,
            disk::disk_write,
            ethernet::ethernet_init,
            ethernet::ethernet_send,
            extfs::list_user_images,
            extfs::read_shared_tree,
            extfs::write_shared_file,
        ])
        .setup(|app| {
            let handle = app.handle();
            copy_bundle_resources(handle)?;
            platform::ensure_shared_dir()?;
            setup_window(handle)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if QUIT_CONFIRMED.load(Ordering::Relaxed) {
                    return;
                }
                api.prevent_close();
                let app = window.app_handle().clone();
                let window = window.clone();
                tauri::async_runtime::spawn(async move {
                    let confirmed = platform::confirm(
                        &app,
                        "Quit macintosh.js?",
                        "All changes to your virtual Macintosh will be lost unless you shut the virtual machine down first.",
                        "Quit anyway",
                        "Cancel",
                    );
                    if confirmed {
                        QUIT_CONFIRMED.store(true, Ordering::Relaxed);
                        app.exit(0);
                    } else {
                        let _ = window.show();
                    }
                });
            }
        })
        .build(tauri::generate_context!())
        .expect("error while running macintosh.js")
        .run(|_, _| {});
}
