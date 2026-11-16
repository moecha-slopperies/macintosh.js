fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "app_info",
            "get_shared_path",
            "boot_disk_info",
            "is_dev_mode",
            "set_dev_mode",
            "reveal_shared_dir",
            "open_license",
            "show_error",
            "toggle_devtools",
            "quit",
            "register_disks",
            "disk_open",
            "disk_close",
            "disk_size",
            "disk_read",
            "disk_write",
            "ethernet_init",
            "ethernet_send",
            "list_user_images",
            "read_shared_tree",
            "write_shared_file",
        ]),
    ))
    .expect("failed to run tauri-build");
}
