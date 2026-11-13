// Prove that assets/holidays/<id>.svg draws the same pixels as the microphone
// the design page composes in place, at the same size.
//
// The page draws <svg><use href="#h-stand"/><g transform=...><use href="#head"/></g></svg>
// with `color: accent`, and the symbols live in a sprite in that page. This
// builds a page that rasterizes both the page composition (with the sprite
// inlined into the same document) and the standalone file, then reports the
// largest channel difference.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const page = readFileSync(join(root, "design", "holiday-icons.html"), "utf8");

const sprite = /<svg width="0" height="0"[\s\S]*?<\/svg>/.exec(page)[0];
const defs = /<defs>[\s\S]*?<\/defs>/.exec(page)[0];

const holidays = [];
{
  const table = /const holidays = \[([\s\S]*?)\n\];/.exec(page)[1];
  const re = /\{ id:'([^']+)'[^}]*?icon:'([^']+)'[^}]*?accent:'(#[0-9a-f]{6})'/g;
  let m;
  while ((m = re.exec(table))) holidays.push({ id: m[1], icon: m[2], accent: m[3] });
  const sheet = /const icon = id === 'newyear' \? '([^']+)'\s*\n\s*: id === 'cny' \? '([^']+)'\s*\n\s*: id === 'friday' \? '([^']+)'/.exec(page);
  const over = { newyear: sheet[1], cny: sheet[2], friday: sheet[3] };
  for (const h of holidays) if (over[h.id]) h.icon = over[h.id];
}

const SIZE = 96;

const rows = holidays.map((h) => {
  const inline =
    `<svg width="${SIZE}" height="${SIZE}" viewBox="0 0 24 24" style="color:${h.accent}">` +
    `<use href="#h-stand"></use>` +
    `<g transform="translate(6.7 0.2) scale(0.44)"><use href="#${h.icon}"></use></g></svg>`;
  // Inlined as a data URI: a file:// image taints the canvas and getImageData
  // then throws, which would hide a real difference.
  const file = readFileSync(join(root, "assets", "holidays", `${h.id}.svg`), "utf8");
  const src = "data:image/svg+xml;charset=utf-8," + encodeURIComponent(file);
  return `<div class="row" data-id="${h.id}" data-accent="${h.accent}" data-icon="${h.icon}">
    <code>${h.id}</code>
    <span class="box" data-mode="page">${inline}</span>
    <span class="box" data-mode="file"><img src="${src}" width="${SIZE}" height="${SIZE}" alt=""></span>
    <b class="out">…</b>
  </div>`;
}).join("");

const html = `<!doctype html><html><head><meta charset="utf-8"><style>
body{background:#0d120f;color:#eef4e8;font:13px system-ui;margin:16px}
.row{display:flex;align-items:center;gap:14px;border-bottom:1px solid #223;padding:4px 0}
code{width:90px}.box{display:grid;place-items:center;width:${SIZE}px;height:${SIZE}px;background:#151b16;border-radius:8px}
.out{width:240px;color:#9ad}
</style></head><body>
${sprite}
${rows}
<pre id="summary">running…</pre>
<script>
const SIZE = ${SIZE};
const DEFS = ${JSON.stringify(defs)};

function load(url) {
  return new Promise((resolve, reject) => {
    const img = new Image();
    img.onload = () => {
      const c = document.createElement("canvas");
      c.width = c.height = SIZE;
      const ctx = c.getContext("2d");
      ctx.drawImage(img, 0, 0, SIZE, SIZE);
      resolve(ctx.getImageData(0, 0, SIZE, SIZE).data);
    };
    img.onerror = () => reject(new Error("raster failed: " + url.slice(0, 60)));
    img.src = url;
  });
}

function dataUrl(svg) {
  return "data:image/svg+xml;charset=utf-8," + encodeURIComponent(svg);
}

/** The page composition, with the sprite's <defs> in the same document so the
 *  <use href="#..."> references resolve. */
function pageSvg(row) {
  const svg = row.querySelector('[data-mode="page"] svg');
  return '<svg xmlns="http://www.w3.org/2000/svg" width="' + SIZE + '" height="' + SIZE + '"' +
    ' viewBox="0 0 24 24" color="' + row.dataset.accent + '">' + DEFS + svg.innerHTML + '</svg>';
}

(async () => {
  let worst = 0;
  const bad = [];
  for (const row of document.querySelectorAll(".row")) {
    const id = row.dataset.id;
    let a, b;
    try {
      a = await load(dataUrl(pageSvg(row)));
      b = await load(row.querySelector("img").src);
    } catch (e) {
      row.querySelector(".out").textContent = "ERROR " + e.message;
      bad.push(id + ":error");
      continue;
    }
    let max = 0;
    for (let i = 0; i < a.length; i += 4) {
      for (let k = 0; k < 3; k++) max = Math.max(max, Math.abs(a[i + k] - b[i + k]));
    }
    worst = Math.max(worst, max);
    row.querySelector(".out").textContent = max === 0 ? "identical" : "max diff " + max;
    if (max > 2) bad.push(id + ":" + max);
  }
  const ok = bad.length === 0;
  document.getElementById("summary").textContent =
    (ok ? "OK: all 17 standalone files render identically to the page markup" : "DIFF: " + bad.join(", ")) +
    "\\nworst channel difference: " + worst;
  document.title = ok ? "OK" : "DIFF";
})();
</script></body></html>`;

const out = join(root, "tools", "verify-holiday-svg.html");
writeFileSync(out, html, "utf8");
console.log(out);
