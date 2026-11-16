import { invoke } from "@tauri-apps/api/core";
import { appInfo, showError } from "./platform.js";

let isDevTools;
const userDirectoryLinks = document.querySelectorAll("#user_dir, #user_dir2");
const devtoolsButton = document.querySelector("#devtools");

async function fetchIsDevTools() {
  await invoke("is_dev_mode").then((result) => {
    isDevTools = result;

    if (result) {
      devtoolsButton.innerHTML = "Disable developer tools";
    } else {
      devtoolsButton.innerHTML = "Enable developer tools";
    }
  });
}

async function help() {
  for (const link of userDirectoryLinks) {
    link.onclick = (event) => {
      event.preventDefault();
      revealSharedDirectory();
    };
  }

  devtoolsButton.onclick = async () => {
    await invoke("set_dev_mode", { enabled: !isDevTools });
    await fetchIsDevTools();
  };

  const info = await appInfo();
  for (const element of userDirectoryLinks) {
    element.textContent = "user directory";
    element.title = info.sharedPath;
  }
  await fetchIsDevTools();
}

function revealSharedDirectory() {
  try {
    invoke("reveal_shared_dir").catch((error) => {
      showError(
        "Could not open folder",
        `We tried to open the macintosh.js directory, but failed. The error was: ${error}`,
      );
    });
  } catch (error) {
    showError(
      "Could not open folder",
      `We tried to open the macintosh.js directory, but failed. The error was: ${error}`,
    );
  }
}

// Keep the helpers local to this page; Tauri commands are async, so clicks
// must not depend on implicit renderer globals.
void (async () => {
  await help();
})();
