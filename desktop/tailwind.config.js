/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        // Surfaces — single neutral ramp, zero hue (Vercel/Geist-style mono).
        // Replaces the old blue-tinted palette; names kept so existing
        // `bg-ink-*` / `border-ink-*` call sites pick up the new values.
        ink: {
          900: "#0a0a0a", // app background
          800: "#0f0f0f", // panels / cards
          700: "#161616", // raised / hover
          600: "#1e1e1e", // hairline borders
          500: "#2a2a2a", // dividers, inputs
          400: "#3a3a3a", // strong border / disabled fill
        },
        // Override Tailwind's cool `zinc` with truly-neutral text grays and a
        // raised contrast floor (old zinc-600 ~#52525b failed WCAG on dark).
        zinc: {
          100: "#ededed", // primary text
          200: "#e0e0e0",
          300: "#cfcfcf",
          400: "#a1a1a1", // secondary text
          500: "#8f8f8f", // muted
          600: "#6f6f6f", // tertiary / faint (floor)
          700: "#4a4a4a",
        },
        // Soft off-white primary action — deliberately not pure #fff.
        primary: { DEFAULT: "#c8c7c2", hover: "#dad9d4" },
        // The one accent hue: active / "working" / info. (Old `teal` names
        // remap here so dots, focus rings, and active highlights all shift.)
        teal: {
          DEFAULT: "#4493f8",
          300: "#8bbcfb",
          400: "#4493f8",
          600: "#2563eb",
        },
        // Status — needs-you.
        amber: {
          DEFAULT: "#d29922",
          300: "#e3b341",
          400: "#d29922",
          500: "#bb8009",
        },
        // Status — done / additions. Override so `emerald-*` reads as our green.
        emerald: {
          300: "#56d364",
          400: "#3fb950",
          500: "#2ea043",
          600: "#238636",
        },
      },
      fontFamily: {
        mono: ["ui-monospace", "SFMono-Regular", "Menlo", "monospace"],
        sans: ["ui-sans-serif", "-apple-system", "system-ui", "sans-serif"],
      },
    },
  },
  plugins: [],
};
