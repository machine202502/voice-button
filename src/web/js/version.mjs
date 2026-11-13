// The version of the phone page, not of the desktop app.
//
// Desktops and phones update separately: a phone can keep an old page in its
// cache while the desktop is already new. The small label in the header shows
// exactly what the phone is running, so "did my page update?" has an answer.
//
// Bump this together with the service-worker shell version in sw.js. Any change
// a person can see on the page needs a bump, otherwise the label lies.

export const SITE_VERSION = '1.15.0';
