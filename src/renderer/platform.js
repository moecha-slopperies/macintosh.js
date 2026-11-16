import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

let infoPromise;

export function appInfo() {
  infoPromise ??= invoke("app_info");
  return infoPromise;
}

export function invokeCommand(command, args) {
  return invoke(command, args);
}

export function onEthernetFrame(callback) {
  return listen("ethernet-receive", (event) => callback(event.payload.frame));
}

export function showError(title, message) {
  return invoke("show_error", { title, message }).catch((error) => {
    console.error(`${title}: ${message}`, error);
  });
}
