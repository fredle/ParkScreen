/**
 * The signalling server. In production the page is on Firebase Hosting
 * (parkscreen.web.app) and the server on Cloud Run, so `VITE_SERVER_URL` is set at build
 * time (e.g. https://parkscreen-server-xxxx.a.run.app). Unset means same origin, which is
 * what local development (the Vite proxy) and a self-hosted server use.
 */
import { wsUrl } from "./url";

const raw = (import.meta.env.VITE_SERVER_URL as string | undefined) ?? "";
export const SERVER_URL = raw.replace(/\/+$/, "");

export const httpUrl = (path: string) => `${SERVER_URL}${path}`;

export const wsEndpoint = (path: string) => wsUrl(path, SERVER_URL || location.origin);
