// Copy the holiday microphones out of design/holiday-icons.html into
// assets/holidays/*.svg.
//
// The page composes every icon as:
//   <use href="#h-stand"/>                                  the microphone stand
//   <g transform="translate(6.7 0.2) scale(0.44)">          the head, shrunk into
//     <use href="#<head>"/>                                 the capsule position
//   </g>
// and sets `color: <accent>` on the container, so `currentColor` in the symbols
// is the holiday accent.
//
// One file per icon is stored, holding the microphone alone: no tile, no baked
// colour. The colour stays `currentColor`, so the phone page can dress it in the
// colour of the record button, while the tray wraps the same file in its tile.
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const page = readFileSync(join(root, "design", "holiday-icons.html"), "utf8");

/** Bodies of every <symbol id="..."> in the page sprite. */
function symbols(html) {
  const out = new Map();
  const re = /<symbol id="([^"]+)"[^>]*>([\s\S]*?)<\/symbol>/g;
  let m;
  while ((m = re.exec(html))) out.set(m[1], m[2].trim());
  return out;
}

/** The holidays table: id, accent, and the head the page picks for the sheet. */
function holidays(html) {
  const out = [];
  const table = /const holidays = \[([\s\S]*?)\n\];/.exec(html)[1];
  const re = /\{ id:'([^']+)'[^}]*?icon:'([^']+)'[^}]*?accent:'(#[0-9a-f]{6})'/g;
  let m;
  while ((m = re.exec(table))) out.push({ id: m[1], icon: m[2], accent: m[3] });
  const sheet = /const icon = id === 'newyear' \? '([^']+)'\s*\n\s*: id === 'cny' \? '([^']+)'\s*\n\s*: id === 'friday' \? '([^']+)'/.exec(html);
  if (sheet) {
    const over = { newyear: sheet[1], cny: sheet[2], friday: sheet[3] };
    for (const h of out) if (over[h.id]) h.icon = over[h.id];
  }
  return out;
}

const sym = symbols(page);
const stand = sym.get("h-stand");
if (!stand) throw new Error("h-stand symbol not found");

const outDir = join(root, "assets", "holidays");
mkdirSync(outDir, { recursive: true });

let written = 0;
const accents = [];
for (const h of holidays(page)) {
  const head = sym.get(h.icon);
  if (!head) throw new Error(`symbol ${h.icon} for ${h.id} not found`);
  // The plain microphone is the whole symbol: its own capsule, bowl and stem.
  const body = h.id === "base"
    ? head
    : `${stand}\n      <g transform="translate(6.7 0.2) scale(0.44)">\n        ${head}\n      </g>`;
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">\n` +
    `      ${body.replaceAll("\n", "\n      ")}\n` +
    `    </svg>\n`;
  const target = h.id === "base" ? join(root, "assets", "icon.svg") : join(outDir, `${h.id}.svg`);
  writeFileSync(target, svg, "utf8");
  if (h.id !== "base") written += 1;
  accents.push(`    ("${h.id}", "${h.accent}"),`);
  console.log(`${h.id.padEnd(12)} <- ${h.icon}  ${h.accent}`);
}

// The accents are quoted by the Rust side to paint the tile behind the very
// same file, so they are printed here instead of being copied by hand.
console.log(`\n${written} icons written to assets/holidays, plus assets/icon.svg`);
console.log("\naccents for src/icons.rs:\n" + accents.join("\n"));
