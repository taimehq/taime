import { inTauri } from "../backend";

/**
 * Open the OS-native folder picker (via tauri-plugin-dialog) and return the
 * chosen absolute path, or null if cancelled / unavailable.
 *
 * Outside the Tauri webview (plain dev browser) there is no native dialog, so
 * this returns null and the caller falls back to manual path entry. The plugin
 * is imported dynamically so it never lands in the browser bundle path.
 */
export async function pickDirectory(defaultPath?: string): Promise<string | null> {
  if (!inTauri()) return null;
  try {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const selected = await open({
      directory: true,
      multiple: false,
      title: "Choose project folder",
      defaultPath: defaultPath || undefined,
    });
    return typeof selected === "string" ? selected : null;
  } catch (e) {
    console.warn("[taime] folder picker failed", e);
    return null;
  }
}
