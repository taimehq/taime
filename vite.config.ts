import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri expects a fixed dev-server port; see tauri.conf.json `build.devUrl`.
const host = process.env.TAURI_DEV_HOST;

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react()],
  // Tauri owns the terminal; don't let Vite wipe Rust/cargo output.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: "ws", host, port: 1421 } : undefined,
    watch: {
      // The Rust side has its own watcher; ignore it here.
      ignored: ["**/src-tauri/**"],
    },
  },
  // Produce a relative-path build so the Tauri webview can load assets.
  base: "./",
});
