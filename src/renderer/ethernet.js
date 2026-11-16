// Renderer-side ethernet plumbing.
//
// Outbound: the worker postMessages each frame the emulated Mac transmits;
// we forward it to the Tauri host over IPC. Inbound: the Rust bridge sends
// frames back as events and we push them into a SharedArrayBuffer ring that
// the worker polls synchronously from inside the WASM main loop (which never
// yields to the JS event queue, so postMessage alone would not work).
//
// The ring layout matches src/basilisk/shared-buffers.js. The
// length-prefixed framing is the same scheme Infinite Mac uses
// (https://github.com/mihaip/infinite-mac, Apache-2.0).

import { invoke } from "@tauri-apps/api/core";
import { acquireLock, releaseLock } from "./atomics.js";
import {
  InputBufferAddresses,
  ETHERNET_HEADER_INTS,
  ETHERNET_BODY_SIZE,
  ETHERNET_BUFFER_SIZE,
} from "../basilisk/shared-buffers.js";
import { onEthernetFrame } from "./platform.js";

export const ethernetReceiveBuffer = new SharedArrayBuffer(
  ETHERNET_BUFFER_SIZE,
);
const header = new Int32Array(ethernetReceiveBuffer, 0, ETHERNET_HEADER_INTS);
const body = new Uint8Array(
  ethernetReceiveBuffer,
  ETHERNET_HEADER_INTS * 4,
  ETHERNET_BODY_SIZE,
);
const bodyLen = body.length;

let inputBufferView = null;

function availableWrite() {
  const w = Atomics.load(header, 0);
  const r = Atomics.load(header, 1);
  return r > w ? r - w - 1 : bodyLen - (w - r) - 1;
}

function pushFrame(frame) {
  const need = frame.length + 2;
  if (need > availableWrite()) {
    console.warn("ethernet: receive ring full, dropping frame");
    return;
  }
  let w = Atomics.load(header, 0);
  body[w] = (frame.length >> 8) & 0xff;
  body[(w + 1) % bodyLen] = frame.length & 0xff;
  w = (w + 2) % bodyLen;
  for (let i = 0; i < frame.length; i++) {
    body[(w + i) % bodyLen] = frame[i];
  }
  Atomics.store(header, 0, (w + frame.length) % bodyLen);

  // Raise the ethernet interrupt flag so the emulator drains the queue
  // promptly instead of waiting for the next idle tick.
  if (
    inputBufferView &&
    acquireLock(inputBufferView, InputBufferAddresses.globalLockAddr)
  ) {
    inputBufferView[InputBufferAddresses.ethernetInterruptFlagAddr] = 1;
    releaseLock(inputBufferView, InputBufferAddresses.globalLockAddr);
  }
}

export function setupEthernet(sharedInputBufferView) {
  inputBufferView = sharedInputBufferView;
  onEthernetFrame((frame) => {
    pushFrame(new Uint8Array(frame));
  });
}

export function handleEthernetInit(macAddress) {
  invoke("ethernet_init", { macAddress }).catch((error) =>
    console.warn("ethernet: could not initialize bridge", error),
  );
}

export function handleEthernetWrite(destination, packet) {
  invoke("ethernet_send", { destination, frame: Array.from(packet) }).catch(
    (error) => console.warn("ethernet: could not send frame", error),
  );
}
