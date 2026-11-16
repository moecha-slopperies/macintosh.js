import { VIDEO_MODE_BUFFER_SIZE } from "../basilisk/shared-buffers.js";

export const videoModeBuffer = new SharedArrayBuffer(
  VIDEO_MODE_BUFFER_SIZE * 4,
);
export const videoModeBufferView = new Int32Array(videoModeBuffer);
export { VIDEO_MODE_BUFFER_SIZE };
