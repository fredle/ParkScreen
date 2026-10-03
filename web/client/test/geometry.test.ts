import { test } from "node:test";
import assert from "node:assert/strict";
import { contentRect, normalise, SlotMap } from "../src/geometry.ts";

test("letterbox: 16:10 video in a wider element has side bars", () => {
  const c = contentRect({ left: 0, top: 0, width: 2000, height: 1000 }, 1600, 1000);
  assert.deepEqual(c, { left: 200, top: 0, width: 1600, height: 1000 });
  assert.deepEqual(normalise(c, 1000, 500), { x: 0.5, y: 0.5, inside: true });
  assert.equal(normalise(c, 100, 500).inside, false);
  assert.equal(normalise(c, 100, 500).x, 0, "clamped");
});

test("pillarbox: tall element has top/bottom bars", () => {
  const c = contentRect({ left: 10, top: 20, width: 1000, height: 1000 }, 1000, 500);
  assert.deepEqual(c, { left: 10, top: 270, width: 1000, height: 500 });
});

test("unknown video size falls back to the element", () => {
  const el = { left: 0, top: 0, width: 300, height: 200 };
  assert.deepEqual(contentRect(el, 0, 0), el);
});

test("slots are small, stable and reused", () => {
  const m = new SlotMap(2);
  assert.equal(m.acquire(1001), 0);
  assert.equal(m.acquire(77), 1);
  assert.equal(m.acquire(1001), 0, "stable");
  assert.equal(m.acquire(5), undefined, "full");
  assert.equal(m.release(1001), 0);
  assert.equal(m.acquire(5), 0, "reused");
});
