import { test } from "node:test";
import assert from "node:assert/strict";
import { wsUrl } from "../src/url.ts";

test("ws URL follows the server's scheme", () => {
  assert.equal(wsUrl("/ws/car", "https://parkscreen-server-x.a.run.app"), "wss://parkscreen-server-x.a.run.app/ws/car");
  assert.equal(wsUrl("/ws/car", "http://localhost:8080"), "ws://localhost:8080/ws/car");
});
