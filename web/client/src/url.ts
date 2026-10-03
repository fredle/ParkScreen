/** `https://host` → `wss://host/path` (pure, so it can be tested without Vite). */
export function wsUrl(path: string, base: string): string {
  return base.replace(/^http/, "ws") + path;
}
