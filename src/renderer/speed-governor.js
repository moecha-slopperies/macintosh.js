// Tracks the emulator's instruction rate and returns a sleep time that keeps
// it near the target. Adapted from Infinite Mac's speed governor.

const ERROR_SMOOTHING_FACTOR = 0.1;

export class SpeedGovernor {
  constructor(targetInstructionsPerSecond) {
    this.lastInstructionCount = 0;
    this.lastTime = performance.now();
    this.targetInstructionsPerMs = targetInstructionsPerSecond / 1000;
    this.errorMs = 0;
  }

  update(instructionCount) {
    const deltaInstructions =
      instructionCount < this.lastInstructionCount
        ? instructionCount
        : instructionCount - this.lastInstructionCount;
    this.lastInstructionCount = instructionCount;

    const now = performance.now();
    const deltaMs = Math.max(now - this.lastTime, 1e-3);
    this.lastTime = now;

    if (this.targetInstructionsPerMs <= 0) return 0;
    const wantedMs = deltaInstructions / this.targetInstructionsPerMs;
    const error = wantedMs - deltaMs;
    this.errorMs =
      ERROR_SMOOTHING_FACTOR * error +
      (1 - ERROR_SMOOTHING_FACTOR) * this.errorMs;
    return Math.max(this.errorMs, 0);
  }
}
