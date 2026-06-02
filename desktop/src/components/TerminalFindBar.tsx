import { useEffect, useRef, useState } from "react";
import { ChevronDown, ChevronUp, X } from "lucide-react";
import type { SearchAddon } from "@xterm/addon-search";
import { SEARCH_DECORATIONS } from "../lib/terminalController";
import type { FindResults } from "../hooks/useTerminalFind";

interface Props {
  searchRef: React.RefObject<SearchAddon | null>;
  queryRef: React.MutableRefObject<string>;
  results: FindResults;
  onClose: () => void;
}

/**
 * A compact find bar pinned to the top-right of a terminal frame. Owns its own
 * query input; drives the frame's `SearchAddon` directly. `queryRef` mirrors
 * the current query so global Cmd+G can repeat the search while this bar is open.
 */
export function TerminalFindBar({
  searchRef,
  queryRef,
  results,
  onClose,
}: Props) {
  const inputRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState(queryRef.current);

  // Focus + select on open so the user can type or replace immediately.
  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  // Clear highlights + the shared query when the bar unmounts (close).
  useEffect(() => {
    return () => {
      queryRef.current = "";
      searchRef.current?.clearDecorations();
    };
  }, [queryRef, searchRef]);

  const search = (q: string, incremental: boolean) => {
    queryRef.current = q;
    if (!q) {
      searchRef.current?.clearDecorations();
      return;
    }
    searchRef.current?.findNext(q, {
      incremental,
      decorations: SEARCH_DECORATIONS,
    });
  };

  const next = () =>
    searchRef.current?.findNext(queryRef.current, {
      decorations: SEARCH_DECORATIONS,
    });
  const prev = () =>
    searchRef.current?.findPrevious(queryRef.current, {
      decorations: SEARCH_DECORATIONS,
    });

  const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      e.shiftKey ? prev() : next();
    } else if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    }
  };

  const count =
    query === ""
      ? ""
      : results.count > 0
        ? `${results.index + 1}/${results.count}`
        : "0/0";

  return (
    <div className="absolute right-3 top-2 z-10 flex items-center gap-1 rounded-md border border-ink-500 bg-ink-800/95 px-1.5 py-1 shadow-lg backdrop-blur">
      <input
        ref={inputRef}
        value={query}
        onChange={(e) => {
          setQuery(e.target.value);
          search(e.target.value, true);
        }}
        onKeyDown={onKeyDown}
        placeholder="Find"
        spellCheck={false}
        className="w-40 bg-transparent px-1 text-xs text-zinc-200 placeholder:text-zinc-500 focus:outline-none"
      />
      <span className="min-w-[44px] text-right text-[11px] tabular-nums text-zinc-500">
        {count}
      </span>
      <button
        onClick={prev}
        title="Previous (Shift+Enter)"
        className="rounded p-0.5 text-zinc-400 hover:bg-ink-600 hover:text-zinc-200"
      >
        <ChevronUp size={14} />
      </button>
      <button
        onClick={next}
        title="Next (Enter)"
        className="rounded p-0.5 text-zinc-400 hover:bg-ink-600 hover:text-zinc-200"
      >
        <ChevronDown size={14} />
      </button>
      <button
        onClick={onClose}
        title="Close (Esc)"
        className="rounded p-0.5 text-zinc-400 hover:bg-ink-600 hover:text-zinc-200"
      >
        <X size={14} />
      </button>
    </div>
  );
}
