/** Where the video content sits inside its element (`object-fit: contain` letterboxing). */
export type Rect = { left: number; top: number; width: number; height: number };

export function contentRect(el: Rect, videoW: number, videoH: number): Rect {
  if (!videoW || !videoH) return el;
  const s = Math.min(el.width / videoW, el.height / videoH);
  const width = videoW * s;
  const height = videoH * s;
  return { left: el.left + (el.width - width) / 2, top: el.top + (el.height - height) / 2, width, height };
}

/** Client coordinates → 0..1 within the video content. `inside` is false over the black bars. */
export function normalise(c: Rect, clientX: number, clientY: number) {
  const x = (clientX - c.left) / c.width;
  const y = (clientY - c.top) / c.height;
  return { x: Math.min(1, Math.max(0, x)), y: Math.min(1, Math.max(0, y)), inside: x >= 0 && x <= 1 && y >= 0 && y <= 1 };
}

/** Pointer ids are mapped to small slots (0..9) because the host rejects ids >= 10. */
export class SlotMap {
  private slots = new Map<number, number>();
  private max: number;
  constructor(max = 10) {
    this.max = max;
  }
  acquire(pointerId: number): number | undefined {
    const have = this.slots.get(pointerId);
    if (have !== undefined) return have;
    const used = new Set(this.slots.values());
    for (let s = 0; s < this.max; s++) {
      if (!used.has(s)) {
        this.slots.set(pointerId, s);
        return s;
      }
    }
    return undefined;
  }
  get(pointerId: number) {
    return this.slots.get(pointerId);
  }
  release(pointerId: number) {
    const s = this.slots.get(pointerId);
    this.slots.delete(pointerId);
    return s;
  }
}
