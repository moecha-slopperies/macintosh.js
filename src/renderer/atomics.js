import { LockStates } from "../basilisk/shared-buffers.js";

export function acquireLock(bufferView, lockIndex) {
  const res = Atomics.compareExchange(
    bufferView,
    lockIndex,
    LockStates.READY_FOR_UI_THREAD,
    LockStates.UI_THREAD_LOCK,
  );

  return res === LockStates.READY_FOR_UI_THREAD;
}

export function releaseLock(bufferView, lockIndex) {
  Atomics.store(bufferView, lockIndex, LockStates.READY_FOR_EMUL_THREAD);
  Atomics.notify(bufferView, lockIndex);
}

export { LockStates };
