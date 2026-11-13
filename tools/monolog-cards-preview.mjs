// A still of the monolog list cards, with the real stylesheet and the same
// markup the page builds, so the outline can be judged before shipping.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const css = readFileSync(join(root, "src", "web", "css", "styles.css"), "utf8");

function card({ name, when, last, count, classes = [], chip = "" }) {
  return `<div class="swipe-row"><div class="monolog-card ${classes.join(" ")}">
    <div class="top"><strong>${name}</strong><span class="when">${when}</span>
      <button class="more" type="button">⋯</button>${chip}</div>
    <div class="last">${last}</div>
    <div class="count">${count}</div>
  </div></div>`;
}

const cards = [
  card({
    name: "Обычный монолог",
    when: "12.11 15:08",
    last: "Всё расшифровано, ничего не висит.",
    count: "12 сообщений",
  }),
  card({
    name: "Идёт расшифровка",
    when: "13.11 18:41",
    last: "Последнее сообщение ещё обрабатывается на компьютере.",
    count: "4 сообщения · 1 в работе",
    classes: ["active"],
  }),
  card({
    name: "Не отправлено",
    when: "13.11 18:44",
    last: "Записано на телефоне, ждёт связи с компьютером.",
    count: "2 сообщения · 1 ждёт связи",
    classes: ["unsent"],
    chip: '<span class="chip unsent">ждёт связи</span>',
  }),
];

const html = `<!doctype html><html lang="ru"><head><meta charset="utf-8"><style>
${css}
body { background: #0d120f; padding: 16px; }
#view { padding: 0; }
h2 { color: #8f9c8b; font-size: 12px; font-weight: 500; margin: 14px 4px 6px; }
</style></head><body><div id="view">
<h2>список монологов — три состояния</h2>
${cards.join("\n")}
</div></body></html>`;

const out = join(root, "design", "monolog-cards-preview.html");
writeFileSync(out, html, "utf8");
console.log(out);
