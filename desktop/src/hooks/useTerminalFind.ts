import { useCallback, useEffect, useRef, useState } from "react";
import type { SearchAddon } from "@xterm/addon-search";
import {
  registerTerminalController,
  SEARCH_DECORATIONS,
} from "../lib/terminalController";

export interface FindResults {
  /** Zero-based index of the active match, or -1 when there are none. */
  index: number;
  count: number;
}

/**
 * Find-in-terminal state + controller wiring, shared by both terminal views
 * (CAO WebSocket and Rust PTY) so the search UX is identical and lives in one
 * place. The view creates the `SearchAddon` (it owns the xterm instance) and
 * stores it in `searchRef`; this hook owns the open/results state, the live
 * query (in a ref so Cmd+G can repeat it), and registers the frame controller.
 */
export function useTerminalFind(frameKey: string) {
  const searchRef = useRef<SearchAddon | null>(null);
  const queryRef = useRef("");
  const [open, setOpen] = useState(false);
  const [results, setResults] = useState<FindResults>({ index: -1, count: 0 });

  const doNext = useCallback(() => {
    if (queryRef.current)
      searchRef.current?.findNext(queryRef.current, {
        decorations: SEARCH_DECORATIONS,
      });
  }, []);

  const doPrev = useCallback(() => {
    if (queryRef.current)
      searchRef.current?.findPrevious(queryRef.current, {
        decorations: SEARCH_DECORATIONS,
      });
  }, []);

  useEffect(() => {
    return registerTerminalController(frameKey, {
      openFind: () => setOpen(true),
      closeFind: () => setOpen(false),
      findNext: doNext,
      findPrev: doPrev,
    });
  }, [frameKey, doNext, doPrev]);

  return { searchRef, queryRef, open, setOpen, results, setResults };
}
