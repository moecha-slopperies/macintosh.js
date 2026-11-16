import { drawScreen } from "./screen.js";
import { tryToSendInput } from "./input.js";
import { registerWorker } from "./worker.js";
import { setupDialogs } from "./dialogs.js";

function asyncLoop() {
  drawScreen();
  tryToSendInput();
  requestAnimationFrame(asyncLoop);
}

async function start() {
  try {
    await registerWorker();
    setupDialogs();
    asyncLoop();
  } catch (error) {
    console.error("Failed to start macintosh.js", error);
  }
}

start();
