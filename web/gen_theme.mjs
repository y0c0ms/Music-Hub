/**
 * Generate the theme token layer for music-hub.
 *
 * Values come from the installed tailwindcss package, not from anything typed by
 * hand, so the dark theme is byte-identical to what the app renders today and the
 * whole change is provably a no-op in dark mode.
 *
 * Two different mappings, because neutrals and accents are not the same problem:
 *
 *   NEUTRALS (slate) mirror. In dark, 950 is the page and 100 is the text; in
 *   light those swap. 500 is the pivot and stays put, which matters because it is
 *   the single most used colour in the app (57 sites, secondary text) and lands at
 *   ~4.8:1 on white.
 *
 *   ACCENTS do NOT mirror. Mirroring would turn `bg-sky-600` (a filled button with
 *   white text) into a pale sky-400 and destroy the contrast. Fills at 500/600/700
 *   are therefore fixed in both themes, and only the two ends move: the light text
 *   shades 300/400 darken so they are readable on white, and the near-black tints
 *   800/900/950 lighten so `bg-red-950` stays a faint wash rather than becoming a
 *   black slab.
 */
import fs from "node:fs";
import colors from "tailwindcss/colors.js";

const SHADES = [50, 100, 200, 300, 400, 500, 600, 700, 800, 900, 950];
const NEUTRAL = ["slate"];
const ACCENT = ["sky", "emerald", "red", "amber"];

const MIRROR = {
  50: 950,
  100: 900,
  200: 800,
  300: 700,
  400: 600,
  500: 500,
  600: 400,
  700: 300,
  800: 200,
  900: 100,
  950: 50,
};
// Accents: only the ends move; 500/600/700 are fills and stay put.
// Everything mirrors EXCEPT the fills at 500/600/700. The pale end (50-200) has
// to mirror too: text-red-100 on bg-red-950/95 is the error toast, and leaving
// 100 pale in light mode put pale text on a pale tint at 1.12:1 - invisible.
//
// 400 -> 700, not 600: measured against the built stylesheet, sky/emerald/amber
// -600 on a light page land at 3.0-3.9:1, which is large-text-only and these are
// 11-13px captions and icon labels. -700 clears 4.5:1 with room to spare.
const ACCENT_LIGHT = {
  50: 950,
  100: 900,
  200: 800,
  300: 800,
  400: 700,
  500: 500,
  600: 600,
  700: 700,
  800: 200,
  900: 100,
  950: 50,
};

const rgb = (hex) => {
  const h = hex.replace("#", "");
  return [0, 2, 4].map((i) => parseInt(h.slice(i, i + 2), 16)).join(" ");
};

let dark = [];
let light = [];
const families = [...NEUTRAL, ...ACCENT];

for (const fam of families) {
  const map = NEUTRAL.includes(fam) ? MIRROR : ACCENT_LIGHT;
  for (const s of SHADES) {
    const v = colors[fam]?.[s];
    if (!v) throw new Error(`missing ${fam}-${s}`);
    dark.push(`    --c-${fam}-${s}: ${rgb(v)};`);
    light.push(`    --c-${fam}-${s}: ${rgb(colors[fam][map[s]])};`);
  }
}

// The Tailwind colour map: every shade reads its variable, and <alpha-value> is
// what keeps the existing opacity modifiers (bg-slate-900/50) working untouched.
const cfg = families
  .map((fam) => {
    const body = SHADES.map(
      (s) => `          ${s}: 'rgb(var(--c-${fam}-${s}) / <alpha-value>)',`,
    ).join("\n");
    return `        ${fam}: {\n${body}\n        },`;
  })
  .join("\n");

fs.writeFileSync("/tmp/theme-vars-dark.css", dark.join("\n"));
fs.writeFileSync("/tmp/theme-vars-light.css", light.join("\n"));
fs.writeFileSync("/tmp/theme-colors.js", cfg);

console.log(`  families: ${families.join(", ")}`);
console.log(`  ${dark.length} variables per theme`);
console.log("  spot checks (dark -> light):");
for (const [f, s] of [
  ["slate", 950],
  ["slate", 900],
  ["slate", 500],
  ["slate", 100],
  ["sky", 400],
  ["sky", 500],
  ["sky", 600],
  ["emerald", 950],
  ["red", 300],
]) {
  const map = NEUTRAL.includes(f) ? MIRROR : ACCENT_LIGHT;
  console.log(
    `    ${f}-${s}: ${colors[f][s]}  ->  ${colors[f][map[s]]}${map[s] === s ? "  (fixed)" : ""}`,
  );
}
