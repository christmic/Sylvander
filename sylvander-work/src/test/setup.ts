globalThis.requestAnimationFrame = (callback: FrameRequestCallback) =>
  globalThis.setTimeout(() => callback(performance.now()), 0);
globalThis.cancelAnimationFrame = (handle: number) => globalThis.clearTimeout(handle);
