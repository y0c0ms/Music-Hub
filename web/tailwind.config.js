/** @type {import('tailwindcss').Config} */
export default {
  // Class-based, not media-based: the app has an explicit Light/Dark/System
  // setting, and 'media' would make System the only reachable option.
  darkMode: "class",
  content: ["./index.html", "./src/**/*.{svelte,js,ts}"],
  theme: {
    extend: {
      colors: {
        // Every neutral and accent shade reads a CSS variable instead of a fixed
        // hex, so the ~110 existing colour classes across the app theme
        // themselves with no edit at each site. <alpha-value> is what keeps the
        // opacity modifiers (bg-slate-900/50, border-slate-800/60) working.
        // The values live in app.css; see there for how light is derived.
        slate: {
          50: "rgb(var(--c-slate-50) / <alpha-value>)",
          100: "rgb(var(--c-slate-100) / <alpha-value>)",
          200: "rgb(var(--c-slate-200) / <alpha-value>)",
          300: "rgb(var(--c-slate-300) / <alpha-value>)",
          400: "rgb(var(--c-slate-400) / <alpha-value>)",
          500: "rgb(var(--c-slate-500) / <alpha-value>)",
          600: "rgb(var(--c-slate-600) / <alpha-value>)",
          700: "rgb(var(--c-slate-700) / <alpha-value>)",
          800: "rgb(var(--c-slate-800) / <alpha-value>)",
          900: "rgb(var(--c-slate-900) / <alpha-value>)",
          950: "rgb(var(--c-slate-950) / <alpha-value>)",
        },
        sky: {
          50: "rgb(var(--c-sky-50) / <alpha-value>)",
          100: "rgb(var(--c-sky-100) / <alpha-value>)",
          200: "rgb(var(--c-sky-200) / <alpha-value>)",
          300: "rgb(var(--c-sky-300) / <alpha-value>)",
          400: "rgb(var(--c-sky-400) / <alpha-value>)",
          500: "rgb(var(--c-sky-500) / <alpha-value>)",
          600: "rgb(var(--c-sky-600) / <alpha-value>)",
          700: "rgb(var(--c-sky-700) / <alpha-value>)",
          800: "rgb(var(--c-sky-800) / <alpha-value>)",
          900: "rgb(var(--c-sky-900) / <alpha-value>)",
          950: "rgb(var(--c-sky-950) / <alpha-value>)",
        },
        emerald: {
          50: "rgb(var(--c-emerald-50) / <alpha-value>)",
          100: "rgb(var(--c-emerald-100) / <alpha-value>)",
          200: "rgb(var(--c-emerald-200) / <alpha-value>)",
          300: "rgb(var(--c-emerald-300) / <alpha-value>)",
          400: "rgb(var(--c-emerald-400) / <alpha-value>)",
          500: "rgb(var(--c-emerald-500) / <alpha-value>)",
          600: "rgb(var(--c-emerald-600) / <alpha-value>)",
          700: "rgb(var(--c-emerald-700) / <alpha-value>)",
          800: "rgb(var(--c-emerald-800) / <alpha-value>)",
          900: "rgb(var(--c-emerald-900) / <alpha-value>)",
          950: "rgb(var(--c-emerald-950) / <alpha-value>)",
        },
        red: {
          50: "rgb(var(--c-red-50) / <alpha-value>)",
          100: "rgb(var(--c-red-100) / <alpha-value>)",
          200: "rgb(var(--c-red-200) / <alpha-value>)",
          300: "rgb(var(--c-red-300) / <alpha-value>)",
          400: "rgb(var(--c-red-400) / <alpha-value>)",
          500: "rgb(var(--c-red-500) / <alpha-value>)",
          600: "rgb(var(--c-red-600) / <alpha-value>)",
          700: "rgb(var(--c-red-700) / <alpha-value>)",
          800: "rgb(var(--c-red-800) / <alpha-value>)",
          900: "rgb(var(--c-red-900) / <alpha-value>)",
          950: "rgb(var(--c-red-950) / <alpha-value>)",
        },
        amber: {
          50: "rgb(var(--c-amber-50) / <alpha-value>)",
          100: "rgb(var(--c-amber-100) / <alpha-value>)",
          200: "rgb(var(--c-amber-200) / <alpha-value>)",
          300: "rgb(var(--c-amber-300) / <alpha-value>)",
          400: "rgb(var(--c-amber-400) / <alpha-value>)",
          500: "rgb(var(--c-amber-500) / <alpha-value>)",
          600: "rgb(var(--c-amber-600) / <alpha-value>)",
          700: "rgb(var(--c-amber-700) / <alpha-value>)",
          800: "rgb(var(--c-amber-800) / <alpha-value>)",
          900: "rgb(var(--c-amber-900) / <alpha-value>)",
          950: "rgb(var(--c-amber-950) / <alpha-value>)",
        },
        brand: {
          50: "#f0f7ff",
          100: "#e0effe",
          500: "#0284c7",
          600: "#0369a1",
          700: "#075985",
          900: "#0c4a6e",
          950: "#082f49",
        },
      },
      // No webfont: this is an installed phone app that has to render instantly
      // and work with the network off, so it uses whatever the OS already has.
      fontFamily: {
        sans: [
          "system-ui",
          "-apple-system",
          "Segoe UI",
          "Roboto",
          "Helvetica Neue",
          "Arial",
          "sans-serif",
        ],
      },
      spacing: {
        safe: "env(safe-area-inset-bottom)",
      },
    },
  },
  plugins: [],
};
