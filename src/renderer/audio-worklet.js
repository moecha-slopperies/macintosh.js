import { RingBuffer } from "./ring-buffer.js";

// Basilisk II emits 384-frame blocks (2 bytes per mono sample). Reading four
// blocks at a time keeps backpressure responsive without adding latency.
const READ_CHUNK_SIZE = 384 * 2 * 4;

class AudioDataProvider {
  constructor(audioBuffer) {
    this.ringBuffer = new RingBuffer(audioBuffer, Uint8Array);
  }

  audioData() {
    const available = this.ringBuffer.availableRead();
    if (available === 0) return undefined;
    const data = new Uint8Array(Math.min(available, READ_CHUNK_SIZE));
    this.ringBuffer.pop(data);
    return data;
  }
}

class EmulatorPlaybackProcessor extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.sampleSize = options.processorOptions.sampleSize;
    this.provider = new AudioDataProvider(
      options.processorOptions.config.audioBuffer,
    );
    this.currentData = undefined;
    this.currentView = undefined;
    this.currentOffset = 0;
    this.errorLogTime = -1;
  }

  process(_inputs, outputs) {
    const output = outputs[0];
    const sampleCount = output[0].length;
    for (let i = 0; i < sampleCount; i++) {
      for (const channel of output) {
        try {
          channel[i] = this._nextSample();
        } catch (error) {
          // Avoid flooding the console if a malformed block arrives.
          if (currentTime - this.errorLogTime > 1) {
            console.error("Could not generate an audio sample", error);
            this.errorLogTime = currentTime;
          }
          channel[i] = 0;
        }
      }
    }
    return true;
  }

  _nextSample() {
    if (!this.currentData || this.currentOffset >= this.currentData.length) {
      this.currentData = this.provider.audioData();
      this.currentView = this.currentData
        ? new DataView(
            this.currentData.buffer,
            this.currentData.byteOffset,
            this.currentData.byteLength,
          )
        : undefined;
      this.currentOffset = 0;
    }
    if (!this.currentData || !this.currentView) return 0;

    let sample;
    switch (this.sampleSize) {
      case 32:
        sample = this.currentView.getFloat32(this.currentOffset, true);
        this.currentOffset += 4;
        break;
      case 16:
        sample = this.currentView.getInt16(this.currentOffset) / 0x8000;
        this.currentOffset += 2;
        break;
      case 8:
        sample = this.currentView.getInt8(this.currentOffset) / 0x80;
        this.currentOffset += 1;
        break;
      default:
        throw new Error(`Unsupported sample size: ${this.sampleSize}`);
    }
    return sample;
  }
}

registerProcessor("emulator-playback-processor", EmulatorPlaybackProcessor);
