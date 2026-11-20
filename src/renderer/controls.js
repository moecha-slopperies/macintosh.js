import { quit, devtools } from "./ipc.js";
import { getIsWorkerRunning, getIsWorkerSaving } from "./worker.js";
import { showCloseWarning } from "./dialogs.js";
import { getIsDevMode } from "./ipc.js";

async function registerControls() {
  document.addEventListener("contextmenu", (event) => {
    event.preventDefault();
  });

  document.querySelector("#close").addEventListener("click", () => {
    if (getIsWorkerSaving()) {
      // Saving already in progress; quit once it completes.
    } else if (getIsWorkerRunning()) {
      showCloseWarning();
    } else {
      quit();
    }
  });

  for (const [selector, page] of [
    ["#credits", "credits.html"],
    ["#help", "help.html"],
  ]) {
    document.querySelector(selector).addEventListener("click", (event) => {
      event.preventDefault();
      window.open(page);
    });
  }

  document.querySelector("#devtools").addEventListener("click", () => {
    devtools();
  });

  if (await getIsDevMode()) {
    document.querySelector("#devtools").classList.remove("hidden");
  }
}

registerControls();
