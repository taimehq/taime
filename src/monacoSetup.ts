/**
 * Point @monaco-editor/react at the LOCALLY BUNDLED monaco-editor instead of its
 * default CDN loader — the Tauri app runs offline, so a CDN fetch would hang the
 * diff view forever. Also wire the Vite-bundled web workers.
 *
 * Per-language workers matter: when the diff shows a `.ts`/`.js`/`.json`/… file,
 * Monaco requests that language's worker by `label`. Returning the generic editor
 * worker for every label makes the language worker fail to load its foreign
 * module (`Cannot read properties of undefined (reading 'toUrl')`). So we route
 * each label to its real worker and fall back to the editor worker otherwise.
 *
 * Imported once for its side effects (see DiffView, which lazy-loads Monaco).
 */
import * as monaco from "monaco-editor";
import editorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import jsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import cssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import htmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import tsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";
import { loader } from "@monaco-editor/react";

self.MonacoEnvironment = {
  getWorker(_workerId: string, label: string) {
    if (label === "json") return new jsonWorker();
    if (label === "css" || label === "scss" || label === "less") return new cssWorker();
    if (label === "html" || label === "handlebars" || label === "razor") return new htmlWorker();
    if (label === "typescript" || label === "javascript") return new tsWorker();
    return new editorWorker();
  },
};

// House theme on the design-system ladder: the code well is the darkest
// surface (#070809, same as the terminal), status green/red for diff tints.
monaco.editor.defineTheme("taime-dark", {
  base: "vs-dark",
  inherit: true,
  rules: [],
  colors: {
    "editor.background": "#070809",
    "editor.foreground": "#c8c7c2",
    "editor.lineHighlightBackground": "#111419",
    "editor.selectionBackground": "#2f4a7a",
    "editorLineNumber.foreground": "#4a505b",
    "editorLineNumber.activeForeground": "#6f7681",
    "editorWidget.background": "#111419",
    "editorWidget.border": "#232936",
    "diffEditor.insertedTextBackground": "#46c46e26",
    "diffEditor.removedTextBackground": "#ef5b5026",
    "diffEditor.insertedLineBackground": "#46c46e14",
    "diffEditor.removedLineBackground": "#ef5b5014",
    "scrollbarSlider.background": "#ffffff14",
    "scrollbarSlider.hoverBackground": "#ffffff24",
    "scrollbarSlider.activeBackground": "#ffffff2e",
  },
});

loader.config({ monaco });
