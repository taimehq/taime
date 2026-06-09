import { defineConfig } from "vitest/config";

// Standalone vitest config (kept separate from vite.config.ts so tests don't
// pull in the React plugin / Tauri dev-server settings). Store tests are pure
// logic — Tauri/localStorage modules are vi.mock'ed — so plain node suffices.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
