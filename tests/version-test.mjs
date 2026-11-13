// The site's own version label. Run with: node tests/version-test.mjs

import { SITE_VERSION } from '../src/web/js/version.mjs';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

let checks = 0;
const failures = [];

function check(condition, message) {
  checks += 1;
  if (condition) {
    console.log('  ok    ' + message);
  } else {
    failures.push(message);
    console.log('  FAIL  ' + message);
  }
}

check(
  /^\d+\.\d+\.\d+$/.test(SITE_VERSION),
  `the site version looks like a version (${SITE_VERSION})`,
);
check(SITE_VERSION !== '0.0.0', 'the first version is not a placeholder');

const here = dirname(fileURLToPath(import.meta.url));
const served = JSON.parse(
  readFileSync(join(here, '..', 'src', 'web', 'version.json'), 'utf8'),
);
check(
  served.site === SITE_VERSION,
  `version.json matches the page (${served.site} / ${SITE_VERSION})`,
);
const sw = readFileSync(join(here, '..', 'src', 'web', 'sw.js'), 'utf8');
check(/voice-button-shell-v\d+/.test(sw), 'the shell has a versioned cache');
check(
  sw.includes("'js/version.mjs'"),
  'the shell caches the version module it shows',
);

console.log('');
if (failures.length > 0) {
  console.log('failed:');
  for (const failure of failures) console.log('  - ' + failure);
  process.exit(1);
}
console.log(`${checks}/${checks} checks passed`);
