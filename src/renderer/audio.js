import { InputBufferAddresses } from "../basilisk/shared-buffers.js";
import { inputBufferView } from "./input.js";
import { RingBuffer } from "./ring-buffer.js";
import audioWorkletUrl from "./audio-worklet.js?worker&url";

// One second of 16-bit mono audio at 22050 Hz. The WASM writes 384-frame
// blocks; keeping a full second lets the worklet absorb scheduling jitter.
const AUDIO_BUFFER_SIZE = 22050 * 2;

export const audioDataBuffer = new SharedArrayBuffer(AUDIO_BUFFER_SIZE);
const audioRingBuffer = new RingBuffer(audioDataBuffer, Uint8Array);

let audioContext;
let playbackNode;
let resumeOnGesture;

function resetAudioBuffer() {
  const count = audioRingBuffer.availableRead();
  if (count > 0) {
    audioRingBuffer.pop(new Uint8Array(count));
  }
}

function markAudioContextRunning() {
  resetAudioBuffer();
  Atomics.store(
    inputBufferView,
    InputBufferAddresses.audioContextRunningFlagAddr,
    1,
  );
}

function stopAudio() {
  Atomics.store(
    inputBufferView,
    InputBufferAddresses.audioContextRunningFlagAddr,
    0,
  );
  if (resumeOnGesture) {
    window.removeEventListener("pointerdown", resumeOnGesture);
    resumeOnGesture = undefined;
  }
  playbackNode?.disconnect();
  playbackNode = undefined;
  if (audioContext) {
    void audioContext.close();
    audioContext = undefined;
  }
}

export async function openAudio(
  sampleRate = 22050,
  sampleSize = 16,
  channels = 1,
) {
  if (typeof AudioContext === "undefined") {
    console.warn("AudioContext is not supported");
    return;
  }
  stopAudio();
  resetAudioBuffer();

  audioContext = new AudioContext({
    latencyHint: "interactive",
    sampleRate,
  });
  if (!audioContext.audioWorklet || typeof AudioWorkletNode === "undefined") {
    console.warn("AudioWorklet is not supported");
    return;
  }

  try {
    await audioContext.audioWorklet.addModule(audioWorkletUrl);
    if (!audioContext || audioContext.state === "closed") return;

    playbackNode = new AudioWorkletNode(
      audioContext,
      "emulator-playback-processor",
      {
        numberOfInputs: 0,
        numberOfOutputs: 1,
        outputChannelCount: [channels],
        channelCount: channels,
        processorOptions: {
          sampleSize,
          config: {
            type: "shared-memory",
            audioBuffer: audioDataBuffer,
          },
        },
      },
    );
    playbackNode.connect(audioContext.destination);
  } catch (error) {
    console.error("Could not initialize audio", error);
    return;
  }

  const handleRunning = () => {
    // Let the worklet start consuming before the worker begins filling the
    // queue. This avoids a large initial burst and avoids stale audio after a
    // guest restart.
    window.setTimeout(markAudioContextRunning, 250);
  };

  resumeOnGesture = () => {
    void audioContext?.resume().catch(() => {});
  };

  if (audioContext.state === "suspended") {
    window.addEventListener("pointerdown", resumeOnGesture);
    audioContext.addEventListener(
      "statechange",
      () => {
        if (audioContext?.state === "running") {
          window.removeEventListener("pointerdown", resumeOnGesture);
          resumeOnGesture = undefined;
          handleRunning();
        }
      },
      { once: true },
    );
    resumeOnGesture();
  } else {
    handleRunning();
  }
}

export { audioRingBuffer };
