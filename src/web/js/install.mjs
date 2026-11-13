// Whether to offer installing the page as an app.
//
// Once the page already runs as the installed app, "Install the app" is noise:
// the footer link and its button are hidden. The decision is a pure function so
// the cases (browser tab, installed app, install event) are testable without a
// browser.

/** Is the page running as an installed app right now? */
export function runningInstalled({ standalone = false, iosStandalone = false, installed = false } = {}) {
  return Boolean(standalone || iosStandalone || installed);
}

/**
 * What the footer shows.
 *
 * `link` is the guide at /install, `button` is the browser's install prompt.
 * The prompt only exists in a browser tab that Chrome considers installable, so
 * the button needs `hasPrompt`; the link does not.
 */
export function installEntry({ standalone = false, iosStandalone = false, installed = false, hasPrompt = false } = {}) {
  const running = runningInstalled({ standalone, iosStandalone, installed });
  return {
    link: !running,
    button: !running && Boolean(hasPrompt),
  };
}

/**
 * Put the decision on the page. `link`, `button` and `about` are elements; a
 * missing one is ignored, because the pairing screen has no footer.
 */
export function applyInstallEntry(entry, { link, button, about } = {}) {
  if (link) link.hidden = !entry.link;
  if (button) button.hidden = !entry.button;
  // Nothing left to show: an empty bar with a border looks like a bug.
  if (about) about.hidden = !entry.link && !entry.button;
  return entry;
}
