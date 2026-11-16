// Emulator worker boot for the Infinite Mac WebAssembly build of Basilisk II.
//
// This file implements the `workerApi` global that the vendored
// `BasiliskII.js`/`BasiliskII.wasm` calls back into for video, input, audio,
// disk, clipboard and ethernet. The shape of that API and the shared-memory
// buffer protocol are taken from Infinite Mac
// (https://github.com/mihaip/infinite-mac, Copyright Mihai Parparita,
// Apache License 2.0 — see LICENSE-infinite-mac). The implementation here is
// macintosh.js-specific: it lets the Tauri host populate and save the extfs
// `macintosh.js` shared folder, and routes disk-image I/O through Tauri.

import {
  InputBufferAddresses,
  LockStates,
  ETHERNET_HEADER_INTS,
  ETHERNET_BODY_SIZE,
} from "./shared-buffers.js";
import {
  listUserImages,
  preloadExtfsTree,
  readSharedTree,
  snapshotExtfsTree,
  sharedDirectory,
  callHost,
} from "./extfs.js";
import {
  configureDisks,
  disksSync as disks,
} from "../renderer/disk-bridge-client.js";
import { RingBuffer } from "../renderer/ring-buffer.js";
import { SpeedGovernor } from "../renderer/speed-governor.js";
import prefsUrl from "./prefs_template?url";
import romUrl from "./rom?url";
import wasmUrl from "./BasiliskII.wasm?url";

let macDir = "";
let Module = null;
let romData;
let wasmBytes;
let extfsEntries = [];
let handledStop = false;
const hostInvoke = callHost;

function wrapExit(imports, moduleName, name) {
  const exit = imports?.[moduleName]?.[name];
  if (typeof exit !== "function") return;

  const wrapped = (status) => {
    handleEmulatorStop(status);
    return exit(status);
  };
  imports[moduleName][name] = wrapped;
}

async function loadAsset(name, url) {
  const response = await fetch(url);
  if (!response.ok) {
    throw new Error(
      `Could not load ${name}: ${response.status} ${response.statusText}`,
    );
  }
  return new Uint8Array(await response.arrayBuffer());
}

// ---------------------------------------------------------------------------
// Ethernet receive ring (worker side). The renderer pushes inbound frames
// into the SharedArrayBuffer; the WASM polls etherRead() each input tick.
// ---------------------------------------------------------------------------

function createEthernetReader(receiveBuffer) {
  if (!receiveBuffer) return () => 0;
  const header = new Int32Array(receiveBuffer, 0, ETHERNET_HEADER_INTS);
  const body = new Uint8Array(
    receiveBuffer,
    ETHERNET_HEADER_INTS * 4,
    ETHERNET_BODY_SIZE,
  );
  const bodyLen = body.length;

  return function read(dest) {
    const writeIdx = Atomics.load(header, 0);
    let readIdx = Atomics.load(header, 1);
    if (readIdx === writeIdx) return 0;

    const hi = body[readIdx];
    const lo = body[(readIdx + 1) % bodyLen];
    const len = (hi << 8) | lo;
    readIdx = (readIdx + 2) % bodyLen;

    if (len > dest.length) {
      // Drop oversized frame but still advance.
      readIdx = (readIdx + len) % bodyLen;
      Atomics.store(header, 1, readIdx);
      return 0;
    }
    for (let i = 0; i < len; i++) {
      dest[i] = body[(readIdx + i) % bodyLen];
    }
    Atomics.store(header, 1, (readIdx + len) % bodyLen);
    return len;
  };
}

// ---------------------------------------------------------------------------
// Prefs
// ---------------------------------------------------------------------------

function buildPrefs(screenWidth, screenHeight, userImages, template) {
  let prefs = new TextDecoder().decode(template).replace(/\r\n/g, "\n");
  prefs += `screen win/${screenWidth}/${screenHeight}\n`;
  for (const img of userImages) {
    prefs += `disk ${img.readOnly ? "*" : ""}${img.name}\n`;
  }
  return new TextEncoder().encode(prefs);
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

self.onmessage = async function (msg) {
  if (msg?.data?.type === "start") {
    startEmulator(msg.data.config);
  }
};

function handleEmulatorStop(status = 0) {
  if (handledStop) return;
  handledStop = true;

  let files = [];
  let error;
  try {
    if (Module?.FS) {
      files = snapshotExtfsTree(Module.FS, "/macintosh.js");
    }
  } catch (snapshotError) {
    error = String(snapshotError?.stack || snapshotError);
  }
  postMessage(
    { type: "emulator_quit", status, hostBase: macDir, files, error },
    files.map(({ data }) => data.buffer),
  );
}

async function startEmulator(config) {
  try {
    macDir = await sharedDirectory();
    const inputBufferView = new Int32Array(config.inputBuffer);
    const screenBufferView = new Uint8Array(config.screenBuffer);
    const videoModeBufferView = new Int32Array(config.videoModeBuffer);
    const audioRingBuffer = new RingBuffer(config.audioDataBuffer, Uint8Array);
    const sleepBuffer = new Int32Array(new SharedArrayBuffer(4));
    const ethernetRead = createEthernetReader(config.ethernetReceiveBuffer);

    const bootDisk = await hostInvoke("boot_disk_info");
    const userImages = await listUserImages(macDir);
    const allDisks = [bootDisk, ...userImages];
    await hostInvoke("register_disks", { disks: allDisks });
    configureDisks(allDisks.map((disk) => disk.name));
    extfsEntries = await readSharedTree(macDir);

    const [template, rom, wasm] = await Promise.all([
      loadAsset("prefs_template", prefsUrl),
      loadAsset("rom", romUrl),
      loadAsset("BasiliskII.wasm", wasmUrl),
    ]);
    romData = rom;
    wasmBytes = wasm;
    const prefs = buildPrefs(
      config.screenWidth,
      config.screenHeight,
      userImages,
      template,
    );

    let lastBlitFrameId = 0;
    let lastIdleWaitFrameId = 0;
    let nextExpectedBlitTime = 0;
    const speedGovernor = new SpeedGovernor(8_000_000);

    const workerApi = {
      InputBufferAddresses,

      idleWait() {
        if (lastIdleWaitFrameId === lastBlitFrameId) return false;
        lastIdleWaitFrameId = lastBlitFrameId;
        const timeout = Math.max(
          0,
          nextExpectedBlitTime - performance.now() - 2,
        );
        const result = Atomics.wait(
          inputBufferView,
          InputBufferAddresses.globalLockAddr,
          LockStates.READY_FOR_UI_THREAD,
          timeout,
        );
        return result === "ok";
      },

      sleep(timeSeconds) {
        Atomics.wait(sleepBuffer, 0, 0, timeSeconds * 1000);
      },

      didOpenVideo(width, height) {
        postMessage({ type: "emulator_video_open", width, height });
      },

      blit(bufPtr, bufSize) {
        lastBlitFrameId++;
        if (bufPtr) {
          const data = Module.HEAPU8.subarray(bufPtr, bufPtr + bufSize);
          videoModeBufferView[0] = bufSize;
          screenBufferView.set(data);
        }
        nextExpectedBlitTime = performance.now() + 16;
      },

      didOpenAudio(sampleRate, sampleSize, channels) {
        postMessage({
          type: "emulator_audio_open",
          sampleRate,
          sampleSize,
          channels,
        });
      },

      audioBufferSize() {
        return audioRingBuffer.availableRead();
      },

      enqueueAudio(bufPtr, nbytes) {
        if (
          !inputBufferView[InputBufferAddresses.audioContextRunningFlagAddr]
        ) {
          return;
        }
        const newAudio = Module.HEAPU8.subarray(bufPtr, bufPtr + nbytes);
        if (audioRingBuffer.availableWrite() < newAudio.byteLength) return;
        audioRingBuffer.push(newAudio);
      },

      acquireInputLock(instructionCount) {
        if (instructionCount !== undefined) {
          const sleepTimeMs = speedGovernor.update(instructionCount);
          if (sleepTimeMs > 0) {
            Atomics.wait(sleepBuffer, 0, 0, sleepTimeMs);
          }
        }
        const res = Atomics.compareExchange(
          inputBufferView,
          InputBufferAddresses.globalLockAddr,
          LockStates.READY_FOR_EMUL_THREAD,
          LockStates.EMUL_THREAD_LOCK,
        );
        return res === LockStates.READY_FOR_EMUL_THREAD ? 1 : 0;
      },

      releaseInputLock() {
        inputBufferView[InputBufferAddresses.mousePositionFlagAddr] = 0;
        inputBufferView[InputBufferAddresses.mousePositionXAddr] = 0;
        inputBufferView[InputBufferAddresses.mousePositionYAddr] = 0;
        inputBufferView[InputBufferAddresses.mouseButtonStateAddr] = 0;
        inputBufferView[InputBufferAddresses.mouseButton2StateAddr] = 0;
        inputBufferView[InputBufferAddresses.keyEventFlagAddr] = 0;
        inputBufferView[InputBufferAddresses.keyCodeAddr] = 0;
        inputBufferView[InputBufferAddresses.keyStateAddr] = 0;
        inputBufferView[InputBufferAddresses.ethernetInterruptFlagAddr] = 0;
        Atomics.store(
          inputBufferView,
          InputBufferAddresses.globalLockAddr,
          LockStates.READY_FOR_UI_THREAD,
        );
      },

      getInputValue(addr) {
        return inputBufferView[addr];
      },

      etherSeed() {
        return Math.floor(Math.random() * 0xffffffff);
      },

      etherInit(macAddress) {
        postMessage({ type: "emulator_ethernet_init", macAddress });
      },

      etherWrite(destination, packetPtr, packetLength) {
        const packet = Module.HEAPU8.slice(packetPtr, packetPtr + packetLength);
        postMessage({ type: "emulator_ethernet_write", destination, packet }, [
          packet.buffer,
        ]);
      },

      etherRead(packetPtr, packetMaxLength) {
        const dest = Module.HEAPU8.subarray(
          packetPtr,
          packetPtr + packetMaxLength,
        );
        return ethernetRead(dest);
      },

      setClipboardText(text) {
        postMessage({ type: "emulator_set_clipboard_text", text });
      },

      getClipboardText() {
        return undefined;
      },

      disks: {
        ...disks,
        read(handle, bufferPtr, offset, length) {
          return disks.read(
            handle,
            Module.HEAPU8.subarray(bufferPtr, bufferPtr + length),
            offset,
            length,
          );
        },
        write(handle, bufferPtr, offset, length) {
          return disks.write(
            handle,
            Module.HEAPU8.subarray(bufferPtr, bufferPtr + length),
            offset,
            length,
          );
        },
      },
    };

    globalThis.workerApi = workerApi;

    const moduleOverrides = {
      arguments: ["--config", "prefs"],

      locateFile: (fileName) => new URL(fileName, import.meta.url).href,

      instantiateWasm(imports, successCallback) {
        wrapExit(imports, "env", "exit");
        wrapExit(imports, "wasi_snapshot_preview1", "proc_exit");
        WebAssembly.instantiate(wasmBytes, imports)
          .then((output) => successCallback(output.instance))
          .catch((error) => {
            console.error("WASM instantiate failed", error);
            postMessage({
              type: "emulator_error",
              error: String(error?.stack || error),
            });
          });
        return {};
      },

      preRun: [
        function () {
          const FS = moduleOverrides.FS;
          FS.createDataFile("/", "prefs", prefs, true, true, true);
          FS.createDataFile("/", "rom", romData, true, true, true);
          FS.mkdir("/macintosh.js");
          preloadExtfsTree(FS, extfsEntries, "/macintosh.js");
          postMessage({ type: "emulator_loading", completion: 1 });
        },
      ],

      onRuntimeInitialized() {
        postMessage({ type: "emulator_ready" });
      },

      print(message) {
        console.log(message);
        postMessage({ type: "TTY", data: message });
      },

      printErr(message) {
        console.warn(message);
        postMessage({ type: "TTY", data: message });
      },

      onExit(status) {
        console.log("Emulator quit with status", status);
        handleEmulatorStop(status);
      },

      quit(status) {
        console.log("Emulator quit with status", status);
        handleEmulatorStop(status);
      },
    };

    Module = moduleOverrides;

    const { default: emulator } = await import("./BasiliskII.js");
    emulator(moduleOverrides);
  } catch (error) {
    console.error("Failed to start emulator", error);
    postMessage({
      type: "emulator_error",
      error: String(error?.stack || error),
    });
  }
}
