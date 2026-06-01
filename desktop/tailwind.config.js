/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      colors: {
        ink: {
          900: "#0b0e13",
          800: "#0e1116",
          700: "#141821",
          600: "#1b2130",
          500: "#252c3d",
          400: "#384256",
        },
        teal: {
          DEFAULT: "#2db6a6",
          400: "#43c6b8",
          600: "#1f9488",
        },
        amber: { DEFAULT: "#e0a458" },
      },
      fontFamily: {
        mono: ["ui-monospace", "SFMono-Regular", "Menlo", "monospace"],
        sans: ["ui-sans-serif", "-apple-system", "system-ui", "sans-serif"],
      },
    },
  },
  plugins: [],
};
