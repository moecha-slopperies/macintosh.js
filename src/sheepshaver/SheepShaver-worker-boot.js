// SheepShaver (PowerPC) worker entry. The shared worker glue lives in
// src/renderer/emulator-worker-boot.js.
import { startEmulatorWorker } from "../renderer/emulator-worker-boot.js";
import prefsUrl from "./prefs_template?url";
import romUrl from "./rom.bin?url";
import wasmUrl from "./SheepShaver.wasm?url";

startEmulatorWorker({
  prefsUrl,
  romUrl,
  wasmUrl,
  wasmName: "SheepShaver.wasm",
  importEmulator: () => import("./SheepShaver.js"),
});
