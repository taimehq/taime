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

loader.config({ monaco });
