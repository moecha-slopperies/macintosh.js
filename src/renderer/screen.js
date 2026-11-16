import { videoModeBufferView } from "./video.js";
import offBackgroundUrl from "./images/off_bg.png?url";

export const SCREEN_WIDTH = 800;
export const SCREEN_HEIGHT = 600;
export const SCREEN_BUFFER_SIZE = SCREEN_WIDTH * SCREEN_HEIGHT * 4; // RGBA

export const screenBuffer = new SharedArrayBuffer(SCREEN_BUFFER_SIZE);
export const screenBufferView = new Uint8Array(screenBuffer);

canvas.width = SCREEN_WIDTH;
canvas.height = SCREEN_HEIGHT;

const canvasCtx = canvas.getContext("2d");
const imageData = canvasCtx.createImageData(SCREEN_WIDTH, SCREEN_HEIGHT);

function fitCanvas() {
  let h = window.innerHeight - 35;
  let w = Math.floor(h * (4 / 3));
  if (w > window.innerWidth) {
    w = window.innerWidth;
    h = Math.floor(w * 0.75);
  }
  canvas.style.width = `${w}px`;
  canvas.style.height = `${h}px`;
}

window.addEventListener("resize", fitCanvas);
fitCanvas();

let stopDrawing = false;

export function drawScreen() {
  if (stopDrawing) return;
  const len = videoModeBufferView[0];
  if (len > 0) {
    imageData.data.set(screenBufferView.subarray(0, len));
    canvasCtx.putImageData(imageData, 0, 0);
  }
}

export function setCanvasBlank() {
  return new Promise((resolve) => {
    stopDrawing = true;
    const ctx = canvas.getContext("2d");
    const bg = new Image();
    let settled = false;

    function finish() {
      if (settled) return;
      settled = true;
      window.clearTimeout(timeout);
      resolve();
    }

    const timeout = window.setTimeout(finish, 2_000);

    ctx.clearRect(0, 0, canvas.width, canvas.height);

    bg.onload = () => {
      const pattern = ctx.createPattern(bg, "repeat");
      if (pattern) {
        ctx.fillStyle = pattern;
        ctx.fillRect(0, 0, canvas.width, canvas.height);
      }
      finish();
    };
    bg.onerror = finish;
    bg.src = offBackgroundUrl;
  });
}
