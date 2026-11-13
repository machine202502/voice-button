// Preview of the record button with the holiday microphone inside it, using the
// page's own colours: the mic inherits `color`, so it follows the button.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const ids = ["halloween", "newyear", "christmas", "love", "friday", "eid"];

const buttons = ids
  .map((id) => {
    const svg = readFileSync(join(root, "assets", "holidays", `${id}.svg`), "utf8");
    return `<div class="case"><div class="row">
      <button class="rec">${svg.replace("<svg ", '<svg class="glyph" ')}</button>
      <button class="rec recording">${svg.replace("<svg ", '<svg class="glyph" ')}</button>
      <button class="rec offline">${svg.replace("<svg ", '<svg class="glyph" ')}</button>
      <span>${id}</span></div></div>`;
  })
  .join("");

const plain = readFileSync(join(root, "src", "web", "index.html"), "utf8")
  .match(/<button id="record"[\s\S]*?<\/button>/)[0]
  .replace('id="record"', 'class="rec"');

const html = `<!doctype html><html><head><meta charset="utf-8"><style>
:root { --lime: #c7f36b; --red: #ff5c5c; --ink: #eef4e8; }
body { margin: 0; padding: 18px; background: #10160f; color: var(--ink); font: 13px system-ui; }
button { border: 0; font: inherit; }
.rec {
  width: 56px; height: 56px; border-radius: 50%;
  background: var(--lime); color: color-mix(in srgb, var(--lime), #0d120f 60%);
  display: grid; place-items: center; flex: 0 0 auto;
}
.rec.recording { background: var(--red); color: color-mix(in srgb, var(--red), #0d120f 60%); }
.rec.offline { background: #e0a13a; color: color-mix(in srgb, #e0a13a, #0d120f 60%); box-shadow: 0 0 0 3px rgba(224,161,58,.22); }
.rec .glyph { width: 30px; height: 30px; display: block; }
.row { display: flex; align-items: center; gap: 14px; margin-bottom: 10px; }
.case { border-bottom: 1px solid #223022; padding-bottom: 6px; }
</style></head><body>
<div class="row">${plain}<span>without a holiday</span></div>
${buttons}
</body></html>`;

const out = join(root, "design", "record-button-preview.html");
writeFileSync(out, html, "utf8");
console.log(out);
