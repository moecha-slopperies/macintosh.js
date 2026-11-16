// Basilisk's WASM disk driver is synchronous, but Tauri IPC is not. The
// emulator worker therefore parks on Atomics.wait() while the main webview
// services a request against the Rust disk host in ../disk-bridge-host.js.
const CONTROL = 0;
const HANDLE = 1;
const OFFSET_LO = 2;
const OFFSET_HI = 3;
const LENGTH = 4;
const RESULT = 5;
const COMMAND = 6;
const REQUEST = 7;
const COMPLETE = 8;
const PENDING = 1;
const OP_OPEN = 1;
const OP_CLOSE = 2;
const OP_SIZE = 3;
const OP_READ = 4;
const OP_WRITE = 5;
export const BRIDGE_CONTROL_INTS = 16;
export const BRIDGE_DATA_BYTES = 4 * 1024 * 1024;
export const BRIDGE_DATA_OFFSET = BRIDGE_CONTROL_INTS * 4;

let control;
let data;
let nextRequest = 1;
let names = [];

function ensureBridge() {
  if (control) return;
  const shared = new SharedArrayBuffer(BRIDGE_DATA_OFFSET + BRIDGE_DATA_BYTES);
  control = new Int32Array(shared, 0, BRIDGE_CONTROL_INTS);
  data = new Uint8Array(shared, BRIDGE_DATA_OFFSET, BRIDGE_DATA_BYTES);
  self.postMessage({
    type: "disk-bridge-init",
    buffer: shared,
    dataLength: BRIDGE_DATA_BYTES,
    names,
  });
}

export function configureDisks(diskNames) {
  names = diskNames.slice();
  if (control) {
    self.postMessage({ type: "disk-bridge-names", names });
  }
}

function request(command, handle, offset, length, bytes) {
  ensureBridge();
  if (length > data.length) return -1;
  const id = nextRequest++;
  Atomics.store(control, HANDLE, handle);
  Atomics.store(control, OFFSET_LO, offset >>> 0);
  Atomics.store(control, OFFSET_HI, Math.floor(offset / 0x1_0000_0000));
  Atomics.store(control, LENGTH, length);
  Atomics.store(control, RESULT, 0);
  Atomics.store(control, COMPLETE, 0);
  if (bytes) data.set(bytes);
  Atomics.store(control, REQUEST, id);
  Atomics.store(control, COMMAND, command);
  Atomics.store(control, CONTROL, PENDING);
  self.postMessage({ type: "disk-bridge-request", id });
  Atomics.notify(control, CONTROL);
  const waited = Atomics.wait(control, COMPLETE, 0, 15_000);
  if (waited === "timed-out") {
    throw new Error("Timed out waiting for the native disk bridge");
  }
  if (Atomics.load(control, COMPLETE) !== id) {
    throw new Error("Native disk bridge returned an out-of-order response");
  }
  return Atomics.load(control, RESULT);
}

export const disksSync = {
  open(name) {
    const index = names.indexOf(name);
    if (index === -1) {
      console.warn(`disks.open: unknown disk "${name}"`);
      return -1;
    }
    return request(OP_OPEN, index, 0, 0);
  },
  close(handle) {
    if (handle >= 0) request(OP_CLOSE, handle, 0, 0);
  },
  size(handle) {
    return request(OP_SIZE, handle, 0, 0);
  },
  read(handle, target, offset, length) {
    const count = request(OP_READ, handle, offset, length);
    if (count > 0) target.set(data.subarray(0, count));
    return count;
  },
  write(handle, source, offset, length) {
    return request(
      OP_WRITE,
      handle,
      offset,
      length,
      source.subarray(0, length),
    );
  },
  isMediaPresent: () => true,
  isFixedDisk: () => true,
  eject() {},
};
