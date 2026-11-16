import { invoke } from "@tauri-apps/api/core";
import {
  BRIDGE_CONTROL_INTS,
  BRIDGE_DATA_OFFSET,
} from "./disk-bridge-client.js";

const CONTROL = 0;
const HANDLE = 1;
const OFFSET_LO = 2;
const OFFSET_HI = 3;
const LENGTH = 4;
const RESULT = 5;
const COMMAND = 6;
const REQUEST = 7;
const COMPLETE = 8;
const OP_OPEN = 1;
const OP_CLOSE = 2;
const OP_SIZE = 3;
const OP_READ = 4;
const OP_WRITE = 5;

let control;
let data;
let names = [];

export function setupDiskBridge(worker) {
  worker.addEventListener("message", (event) => {
    const message = event.data;
    if (message?.type === "disk-bridge-init") {
      control = new Int32Array(message.buffer, 0, BRIDGE_CONTROL_INTS);
      data = new Uint8Array(
        message.buffer,
        BRIDGE_DATA_OFFSET,
        message.dataLength,
      );
      names = message.names || [];
    } else if (message?.type === "disk-bridge-names") {
      names = message.names || [];
    } else if (message?.type === "disk-bridge-request") {
      void respond(message.id);
    } else if (message?.type === "host-invoke") {
      void hostInvoke(message, worker);
    }
  });
}

async function respond(id) {
  if (!control) return;
  const command = Atomics.load(control, COMMAND);
  const value = Atomics.load(control, HANDLE);
  const offset =
    Atomics.load(control, OFFSET_HI) * 0x1_0000_0000 +
    (Atomics.load(control, OFFSET_LO) >>> 0);
  const length = Atomics.load(control, LENGTH);
  let result = 0;

  try {
    if (command === OP_OPEN) {
      result = await invoke("disk_open", { name: names[value] });
    } else if (command === OP_CLOSE) {
      await invoke("disk_close", { handle: value });
    } else if (command === OP_SIZE) {
      result = await invoke("disk_size", { handle: value });
    } else if (command === OP_READ) {
      const bytes = new Uint8Array(
        await invoke("disk_read", { handle: value, offset, length }),
      );
      data.set(bytes.subarray(0, length));
      result = Math.min(bytes.length, length);
    } else if (command === OP_WRITE) {
      const bytes = data.slice(0, length);
      result = await invoke("disk_write", bytes, {
        headers: {
          "x-disk-handle": String(value),
          "x-disk-offset": String(offset),
        },
      });
    }
  } catch (error) {
    console.error("disk bridge command failed", error);
    result = -1;
  }

  Atomics.store(control, RESULT, result);
  Atomics.store(control, COMPLETE, id);
  Atomics.notify(control, COMPLETE);
}

async function hostInvoke(message, worker) {
  try {
    const result = message.rawData
      ? await invoke(message.command, message.rawData, {
          headers: message.headers || {},
        })
      : await invoke(message.command, message.args || {});
    worker.postMessage({ type: "host-invoke-result", id: message.id, result });
  } catch (error) {
    worker.postMessage({
      type: "host-invoke-result",
      id: message.id,
      error: String(error),
    });
  }
}
