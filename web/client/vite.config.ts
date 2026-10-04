import { defineConfig } from "vite";
import { resolve } from "node:path";
export default defineConfig({
  // `/` is the intro page, `/car/` is the pairing and viewing app.
  build: {
    target: "es2020",
    assetsDir: "assets",
    rollupOptions: { input: { main: resolve(__dirname, "index.html"), car: resolve(__dirname, "car/index.html") } },
  },
  server: { proxy: { "/api": "http://localhost:8080", "/ws": { target: "ws://localhost:8080", ws: true } } },
});
