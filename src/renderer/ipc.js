import { invoke } from "@tauri-apps/api/core";

export function quit() {
  return invoke("quit");
}

export function devtools() {
  return invoke("toggle_devtools");
}

export function getIsDevMode() {
  return invoke("is_dev_mode");
}
