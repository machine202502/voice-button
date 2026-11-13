// Every supported language must carry every key, in both catalogs: the
// desktop window and the phone/Telegram messages.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
let checks = 0;
const failures = [];
function check(ok, label) {
  checks += 1;
  if (ok) console.log('  ok   ', label);
  else {
    failures.push(label);
    console.log('  FAIL ', label);
  }
}

function keys(file) {
  const data = JSON.parse(fs.readFileSync(file, 'utf8'));
  return Object.keys(data.strings || data);
}

const webIndex = JSON.parse(
  fs.readFileSync(path.join(root, 'src/web/lang/index.json'), 'utf8'),
);
const webFiles = fs
  .readdirSync(path.join(root, 'src/web/lang'))
  .filter((name) => name.endsWith('.json') && name !== 'index.json' && !name.startsWith('install.'))
  .map((name) => name.replace(/\.json$/, ''))
  .sort();
check(
  JSON.stringify(webIndex.languages.map((item) => item.code).sort()) === JSON.stringify(webFiles),
  'lang/index.json lists every language file and nothing else',
);
for (const item of webIndex.languages) {
  const install = path.join(root, 'src/web/lang', 'install.' + item.code + '.json');
  check(fs.existsSync(install), 'install guide exists for ' + item.code);
  if (fs.existsSync(install)) {
    const data = JSON.parse(fs.readFileSync(install, 'utf8'));
    check(
      Boolean(data.title && data.html && data.html.includes('<h1>')),
      'install guide for ' + item.code + ' has a title and content',
    );
  }
}

for (const folder of ['locales', 'src/web/lang']) {
  const files = ['ru', 'en', 'zh'].map((code) => path.join(root, folder, code + '.json'));
  const sets = files.map(keys);
  const union = new Set(sets.flat());
  for (let index = 0; index < sets.length; index += 1) {
    const missing = [...union].filter((key) => !sets[index].includes(key));
    check(missing.length === 0, folder + '/' + ['ru', 'en', 'zh'][index] + ' has every key');
    const parsed = JSON.parse(fs.readFileSync(files[index], 'utf8'));
    const data = parsed.strings || parsed;
    check(
      Object.values(data).every((value) => typeof value === 'string' && value.trim().length > 0),
      folder + '/' + ['ru', 'en', 'zh'][index] + ' has no empty translations',
    );
  }
}

// Every literal key used by the phone and the desktop/bot Rust code must
// exist in all three catalogs. This is what catches a new hardcoded string.
const phone = fs.readFileSync(path.join(root, 'src/web/js/app.js'), 'utf8');
const phoneKeys = new Set(
  [...phone.matchAll(/\bt\('([^']+)'/g)].map((match) => match[1]).filter((key) => key !== 'messages_'),
);
for (const code of ['ru', 'en', 'zh']) {
  const catalog = JSON.parse(
    fs.readFileSync(path.join(root, 'src/web/lang', code + '.json'), 'utf8'),
  );
  const missing = [...phoneKeys].filter((key) => !(key in catalog));
  check(missing.length === 0, 'phone/' + code + ' has every used key');
}

for (const source of ['src/ui.rs', 'src/telegram.rs']) {
  const rust = fs.readFileSync(path.join(root, source), 'utf8');
  const keys = [...rust.matchAll(/\btrf?\("([^"]+)"/g)].map((match) => match[1]);
  for (const code of ['ru', 'en', 'zh']) {
    const catalog = JSON.parse(
      fs.readFileSync(path.join(root, 'locales', code + '.json'), 'utf8'),
    ).strings;
    const missing = keys.filter((key) => !(key in catalog));
    check(missing.length === 0, source + '/' + code + ' has every used key');
  }
}

console.log();
console.log((checks - failures.length) + '/' + checks + ' checks passed');
if (failures.length) process.exit(1);
