// The version of the phone page.
//
// The number lives in `public/version.json`, which is both copied into the build
// (the computer and the tests read it there) and imported here, so Vite bakes it
// into the bundle. The header therefore shows the version on the first paint,
// with no request: a label that waited for a fetch was empty whenever the network
// was slow or gone, which is exactly the case this page exists for.

import versionJson from '../../public/version.json';

/** The version this build was made from, e.g. "1.17.4". */
export function version(): string {
  return versionJson.site;
}
