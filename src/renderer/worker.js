import { invoke } from "@tauri-apps/api/core";
import { inputBuffer, inputBufferView } from "./input.js";
import { videoModeBuffer } from "./video.js";
import {
  screenBuffer,
  SCREEN_WIDTH,
  SCREEN_HEIGHT,
  setCanvasBlank,
} from "./screen.js";
import { audioDataBuffer, openAudio } from "./audio.js";
import {
  ethernetReceiveBuffer,
  setupEthernet,
  handleEthernetInit,
  handleEthernetWrite,
} from "./ethernet.js";
import { quit } from "./ipc.js";
import { showError } from "./platform.js";
import { setupDiskBridge } from "./disk-bridge-host.js";

let isWorkerRunning = false;
let isWorkerSaving = false;
let worker;

export function getIsWorkerRunning() {
  return isWorkerRunning;
}

export function getIsWorkerSaving() {
  return isWorkerSaving;
}

async function saveExtfsSnapshot(hostBase, files) {
  for (const file of files) {
    await invoke("write_shared_file", {
      hostBase,
      path: file.path,
      data: Array.from(file.data),
    });
  }
}

async function handleWorkerShutdown(msg) {
  isWorkerRunning = false;
  isWorkerSaving = true;
  document.body.classList.remove("emulator_running");
  await setCanvasBlank();
  document.querySelector("#disk_saving").classList.remove("hidden");

  try {
    if (msg.error) throw new Error(msg.error);
    await saveExtfsSnapshot(msg.hostBase, msg.files || []);
  } catch (error) {
    console.error("Could not save files from the virtual machine", error);
    showError(
      "Could not save files",
      `We tried to save files from the virtual machine, but failed. The error was: ${error}`,
    );
  } finally {
    isWorkerSaving = false;
  }

  quit();
}

export async function registerWorker() {
  setupEthernet(inputBufferView);

  const config = {
    inputBuffer,
    screenBuffer,
    videoModeBuffer,
    audioDataBuffer,
    ethernetReceiveBuffer,
    screenWidth: SCREEN_WIDTH,
    screenHeight: SCREEN_HEIGHT,
  };

  // The worker entry is resolved by core in vite.config.js; a runtime branch
  // would bundle both emulator cores into every build.
  worker = window.emulatorWorker = new Worker(
    new URL("@emulator-worker-entry", import.meta.url),
    { type: "module" },
  );
  setupDiskBridge(worker);

  isWorkerRunning = true;

  worker.onmessage = function (e) {
    const msg = e.data;
    switch (msg?.type) {
      case "emulator_loading": {
        const progressElement = document.querySelector("#progressbar");
        if (progressElement) {
          progressElement.value = Math.max(10, msg.completion * 100);
          progressElement.max = 100;
        }
        break;
      }
      case "emulator_video_open":
        document.body.classList.remove("emulator_loading");
        document.body.classList.add("emulator_running");
        break;
      case "emulator_audio_open":
        void openAudio(msg.sampleRate, msg.sampleSize, msg.channels);
        break;
      case "emulator_quit":
        void handleWorkerShutdown(msg);
        break;
      case "emulator_ethernet_init":
        handleEthernetInit(msg.macAddress);
        break;
      case "emulator_ethernet_write":
        handleEthernetWrite(msg.destination, msg.packet);
        break;
      case "emulator_set_clipboard_text":
        navigator.clipboard?.writeText(msg.text).catch(() => {});
        break;
      case "emulator_error":
        console.error("Emulator error", msg.error);
        showError("Emulator error", msg.error);
        break;
      case "showMessageBoxSync":
        showError(
          msg.options?.title || "macintosh.js",
          msg.options?.message || String(msg.options),
        );
        break;
      case "TTY":
        // Logged in the worker; nothing to do here.
        break;
    }
  };
  worker.postMessage({ type: "start", config });
}
