// Single-producer/single-consumer ring buffer for audio data. The layout is
// compatible with ringbuf.js: two uint32 indices followed by Uint8 storage.
// Adapted from ringbuf.js (MIT) and Infinite Mac's audio buffering.

export class RingBuffer {
  constructor(sharedBuffer, type = Uint8Array) {
    if (!(sharedBuffer instanceof SharedArrayBuffer)) {
      throw new TypeError("RingBuffer requires a SharedArrayBuffer");
    }
    this.type = type;
    this.capacity = (sharedBuffer.byteLength - 8) / type.BYTES_PER_ELEMENT;
    this.buffer = sharedBuffer;
    this.writePointer = new Uint32Array(sharedBuffer, 0, 1);
    this.readPointer = new Uint32Array(sharedBuffer, 4, 1);
    this.storage = new type(sharedBuffer, 8, this.capacity);
  }

  availableRead() {
    const read = Atomics.load(this.readPointer, 0);
    const write = Atomics.load(this.writePointer, 0);
    return (write + this.capacity - read) % this.capacity;
  }

  availableWrite() {
    return this.capacity - 1 - this.availableRead();
  }

  empty() {
    return this.availableRead() === 0;
  }

  push(elements, length = elements.length) {
    const read = Atomics.load(this.readPointer, 0);
    const write = Atomics.load(this.writePointer, 0);
    const available = this.capacity - 1 - this._availableRead(read, write);
    const count = Math.min(available, length);
    const first = Math.min(this.capacity - write, count);
    this._copy(elements, 0, this.storage, write, first);
    this._copy(elements, first, this.storage, 0, count - first);
    Atomics.store(this.writePointer, 0, (write + count) % this.capacity);
    return count;
  }

  pop(elements, length = elements.length) {
    const read = Atomics.load(this.readPointer, 0);
    const write = Atomics.load(this.writePointer, 0);
    const count = Math.min(this._availableRead(read, write), length);
    const first = Math.min(this.capacity - read, count);
    this._copy(this.storage, read, elements, 0, first);
    this._copy(this.storage, 0, elements, first, count - first);
    Atomics.store(this.readPointer, 0, (read + count) % this.capacity);
    return count;
  }

  _availableRead(read, write) {
    return (write + this.capacity - read) % this.capacity;
  }

  _copy(input, inputOffset, output, outputOffset, count) {
    for (let i = 0; i < count; i++) {
      output[outputOffset + i] = input[inputOffset + i];
    }
  }
}
