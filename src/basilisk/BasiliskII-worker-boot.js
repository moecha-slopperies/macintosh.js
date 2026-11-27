// Basilisk II (68K) worker entry. The shared worker glue lives in
// src/renderer/emulator-worker-boot.js.
import { startEmulatorWorker } from "../renderer/emulator-worker-boot.js";
import prefsUrl from "./prefs_template?url";
import romUrl from "./rom.bin?url";
import wasmUrl from "./BasiliskII.wasm?url";

startEmulatorWorker({
  prefsUrl,
  romUrl,
  wasmUrl,
  wasmName: "BasiliskII.wasm",
  targetIps: 8_000_000,
  importEmulator: () => import("./BasiliskII.js"),
});
