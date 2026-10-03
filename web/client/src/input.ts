import { contentRect, normalise, SlotMap } from "./geometry";

type Send = (msg: object) => void;

/**
 * Turns pointer/touch/wheel events on the video into the host's `input` channel messages
 * (normalised coordinates; see host/src/input.rs). Moves are coalesced to one per frame.
 * Returns a detach function.
 */
export function attachInput(video: HTMLVideoElement, send: Send): () => void {
  const slots = new SlotMap(10);
  const pending = new Map<number, { x: number; y: number }>();
  let raf = 0;

  const pos = (e: PointerEvent | WheelEvent) => {
    const r = video.getBoundingClientRect();
    return normalise(contentRect(r, video.videoWidth, video.videoHeight), e.clientX, e.clientY);
  };
  const flush = () => {
    raf = 0;
    for (const [id, p] of pending) send({ t: "move", id, ...p });
    pending.clear();
  };

  const down = (e: PointerEvent) => {
    const p = pos(e);
    if (!p.inside) return;
    const id = slots.acquire(e.pointerId);
    if (id === undefined) return;
    e.preventDefault();
    video.setPointerCapture(e.pointerId);
    // First gesture: go full-screen (needs a user gesture) so coordinates match the panel.
    if (!document.fullscreenElement) video.requestFullscreen?.().catch(() => {});
    send({ t: "down", id, x: p.x, y: p.y });
  };
  const move = (e: PointerEvent) => {
    const id = slots.get(e.pointerId);
    if (id === undefined) return;
    const p = pos(e);
    pending.set(id, { x: p.x, y: p.y });
    if (!raf) raf = requestAnimationFrame(flush);
  };
  const up = (e: PointerEvent) => {
    const id = slots.release(e.pointerId);
    if (id === undefined) return;
    flush(); // deliver the final position before the release
    send({ t: "up", id });
  };
  const wheel = (e: WheelEvent) => {
    e.preventDefault();
    // Wheel deltas are pixels (line/page modes are scaled); sign follows the usual convention.
    const k = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 400 : 1;
    send({ t: "scroll", dx: e.deltaX * k, dy: e.deltaY * k });
  };
  const noMenu = (e: Event) => e.preventDefault();

  video.style.touchAction = "none";
  video.addEventListener("pointerdown", down);
  video.addEventListener("pointermove", move);
  video.addEventListener("pointerup", up);
  video.addEventListener("pointercancel", up);
  video.addEventListener("wheel", wheel, { passive: false });
  video.addEventListener("contextmenu", noMenu);
  return () => {
    video.removeEventListener("pointerdown", down);
    video.removeEventListener("pointermove", move);
    video.removeEventListener("pointerup", up);
    video.removeEventListener("pointercancel", up);
    video.removeEventListener("wheel", wheel);
    video.removeEventListener("contextmenu", noMenu);
    if (raf) cancelAnimationFrame(raf);
  };
}
