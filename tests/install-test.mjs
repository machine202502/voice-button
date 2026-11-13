// The footer's install entry: shown in a browser tab, hidden in the installed
// app. Run with: node tests/install-test.mjs

import { applyInstallEntry, installEntry, runningInstalled } from '../src/web/js/install.mjs';

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

// ---------------------------------------------------------------- detection

check(runningInstalled({ standalone: true }) === true, 'display-mode standalone means installed');
check(runningInstalled({ iosStandalone: true }) === true, 'iPhone Safari standalone means installed');
check(runningInstalled({ installed: true }) === true, 'the appinstalled event means installed');
check(
  runningInstalled({}) === false,
  'a plain browser tab is not the installed app',
);
check(
  runningInstalled({ standalone: false, iosStandalone: false, installed: false }) === false,
  'explicit false stays false',
);

// ---------------------------------------------------------------- entry

const tab = installEntry({ hasPrompt: true });
check(tab.link === true, 'a browser tab offers the install guide');
check(tab.button === true, 'and the prompt button when Chrome offers one');

const tabWithoutPrompt = installEntry({ hasPrompt: false });
check(
  tabWithoutPrompt.link === true && tabWithoutPrompt.button === false,
  'without a prompt the guide stays and the button goes',
);

const installedApp = installEntry({ standalone: true, hasPrompt: true });
check(
  installedApp.link === false && installedApp.button === false,
  'the installed app offers neither the guide nor the prompt',
);

const installedEvent = installEntry({ installed: true, hasPrompt: true });
check(
  installedEvent.link === false && installedEvent.button === false,
  'after appinstalled the entry disappears at once',
);

const iphone = installEntry({ iosStandalone: true, hasPrompt: true });
check(
  iphone.link === false && iphone.button === false,
  'the iPhone app offers neither',
);

// ---------------------------------------------------------------- the page

const page = () => ({
  link: { hidden: false },
  button: { hidden: false },
  about: { hidden: false },
});

const browserTab = page();
applyInstallEntry(installEntry({ hasPrompt: true }), browserTab);
check(
  browserTab.link.hidden === false && browserTab.button.hidden === false,
  'a browser tab shows the guide and the prompt',
);
check(browserTab.about.hidden === false, 'and keeps the footer');

const installedPage = page();
applyInstallEntry(installEntry({ standalone: true, hasPrompt: true }), installedPage);
check(
  installedPage.link.hidden === true && installedPage.button.hidden === true,
  'the installed app hides both entries',
);
check(
  installedPage.about.hidden === true,
  'and the empty footer with them, so no bare bar is left',
);

const pairing = { link: null, button: null, about: null };
applyInstallEntry(installEntry({ hasPrompt: false }), pairing);
check(true, 'a missing footer is not an error (the pairing screen)');

console.log('');
if (failures.length > 0) {
  console.log('failed:');
  for (const failure of failures) console.log('  - ' + failure);
  process.exit(1);
}
console.log(`${checks}/${checks} checks passed`);
