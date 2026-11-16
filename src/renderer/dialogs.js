import { quit } from "./ipc.js";

export function setupDialogs() {
  // Still empty
}

export function showCloseWarning() {
  const warningDialog = document.querySelector("#warning");

  document.querySelector("#warning-quit").onclick = () => {
    document.querySelector("#warning").classList.add("hidden");
    quit();
  };

  document.querySelector("#warning-cancel").onclick = () => {
    warningDialog.classList.add("hidden");
  };

  warningDialog.classList.remove("hidden");
}
