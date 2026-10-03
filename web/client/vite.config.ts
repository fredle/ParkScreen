import { defineConfig } from "vite";
export default defineConfig({
  build: { target: "es2020", assetsDir: "assets" },
  server: { proxy: { "/api": "http://localhost:8080", "/ws": { target: "ws://localhost:8080", ws: true } } },
});
