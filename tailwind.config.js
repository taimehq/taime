/** @type {import('tailwindcss').Config} */

// The one accent hue: active / "working" / info — muted developer-tool blue.
// Old `teal` names remap here so dots, focus rings, and active highlights all
// shift; `accent` is the first-class alias new code should prefer.
const accent = {
  DEFAULT: "#5b8def",
  300: "#8db1f5",
  400: "#5b8def",
  500: "#4a7ad6", // press shade
  600: "#3f6fd9",
};

export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        // Surfaces — deep charcoal with a cold blue cast (design-system bg
        // ladder). Names kept so existing `bg-ink-*` / `border-ink-*` call
        // sites pick up the new values.
        ink: {
          900: "#08090c", // app backdrop, deepest wells (bg-0)
          800: "#0c0e13", // primary workspace surface (bg-1)
          700: "#111419", // panels, sidebars, cards (bg-2)
          600: "#161a21", // raised cards, selected rows (bg-3)
          500: "#1c212a", // hover / popover / input wells (bg-4)
          400: "#232936", // strong border / disabled fill
        },
        // Text grays — warm off-white primary (the locked brand color) easing
        // into the design system's cool fg scale further down the ramp.
        zinc: {
          100: "#c8c7c2", // primary text (brand off-white)
          200: "#b8b7b2",
          300: "#aeb0b4",
          400: "#a4abb6", // secondary text (fg-2)
          500: "#8a919d", // muted
          600: "#6f7681", // tertiary / faint floor (fg-3)
          700: "#4a505b", // disabled, placeholders (fg-4)
        },
        // Primary action — the accent blue (white text on top, not ink).
        primary: { DEFAULT: "#5b8def", hover: "#6b9af1" },
        teal: accent,
        accent,
        // Status — needs-you.
        amber: {
          DEFAULT: "#e3a93a",
          300: "#eec272",
          400: "#e3a93a",
          500: "#c08a22",
          600: "#8a6418",
        },
        // Status — done / additions. Override so `emerald-*` reads as our green.
        emerald: {
          300: "#6fd193",
          400: "#46c46e",
          500: "#36a458",
          600: "#2a8147",
        },
        // Status — failed / deletions. `red` and `rose` collapse to one family.
        red: {
          DEFAULT: "#ef5b50",
          300: "#f48a82",
          400: "#ef5b50",
          500: "#d9453a",
          900: "#8a2f29",
        },
        rose: {
          DEFAULT: "#ef5b50",
          200: "#f8b1ab",
          300: "#f48a82",
          400: "#ef5b50",
          500: "#d9453a",
        },
        // Status — agent / AI accent.
        violet: {
          DEFAULT: "#9a7cf0",
          300: "#b7a1f5",
          400: "#9a7cf0",
          500: "#7e5fe0",
        },
      },
      borderColor: {
        hairline: "rgba(255, 255, 255, 0.08)",
      },
      // The design system's small-radius family (xs 3 / sm 5 / md 7 / lg 10)
      // mapped onto the existing utility names: chips `rounded`, controls
      // `rounded-md`, cards/panels `rounded-lg`, dialogs `rounded-xl`.
      borderRadius: {
        DEFAULT: "3px",
        md: "5px",
        lg: "7px",
        xl: "10px",
      },
      fontFamily: {
        sans: [
          "Geist",
          "-apple-system",
          "BlinkMacSystemFont",
          "system-ui",
          "sans-serif",
        ],
        mono: [
          "'Geist Mono'",
          "ui-monospace",
          "SFMono-Regular",
          "Menlo",
          "monospace",
        ],
      },
    },
  },
  plugins: [],
};
