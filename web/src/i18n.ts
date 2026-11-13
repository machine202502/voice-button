// Interface translations for the phone page.
//
// The catalogs are the same JSON files the old page used (`lang/<code>.json`,
// short keys like `no_messages`). They are copied into the build by
// `scripts/web.ps1`, so this module only has to fetch one and answer `t()`.
//
// The language follows the phone, which is the only sensible default: a person
// reading the page set their phone to that language. English is the fallback for
// a language without a catalog, and for a key missing from one.

export type LanguageCode = string;

export interface LanguageEntry {
  code: LanguageCode;
  name: string;
}

type Catalog = Record<string, string>;

const FALLBACK: LanguageCode = 'en';

let available: LanguageEntry[] = [{ code: FALLBACK, name: 'English' }];
let catalogs: Record<LanguageCode, Catalog> = {};
let current: LanguageCode = FALLBACK;

/** The language the page should use: the phone's, if a catalog exists for it. */
export function languageFromPhone(): LanguageCode {
  const candidates: string[] = [];
  if (typeof navigator !== 'undefined') {
    if (Array.isArray(navigator.languages)) candidates.push(...navigator.languages);
    if (navigator.language) candidates.push(navigator.language);
  }
  for (const candidate of candidates) {
    const base = candidate.split('-')[0]?.toLowerCase();
    if (base && available.some((entry) => entry.code === base)) return base;
  }
  return FALLBACK;
}

async function fetchCatalog(code: LanguageCode): Promise<Catalog> {
  try {
    const response = await fetch(`./lang/${code}.json`);
    if (!response.ok) return {};
    const parsed: unknown = await response.json();
    if (!parsed || typeof parsed !== 'object') return {};
    const catalog: Catalog = {};
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof value === 'string') catalog[key] = value;
    }
    return catalog;
  } catch {
    return {};
  }
}

/**
 * Load the language list and the catalog for `wanted` (or the phone's language).
 * Called once at boot; `t()` is only reliable after this resolves.
 */
export async function loadLanguage(wanted?: LanguageCode): Promise<LanguageCode> {
  try {
    const response = await fetch('./lang/index.json');
    if (response.ok) {
      const parsed: unknown = await response.json();
      const list = (parsed as { languages?: unknown }).languages;
      if (Array.isArray(list)) {
        available = list
          .filter(
            (item): item is LanguageEntry =>
              !!item &&
              typeof item === 'object' &&
              typeof (item as LanguageEntry).code === 'string' &&
              typeof (item as LanguageEntry).name === 'string',
          )
          .map((item) => ({ code: item.code, name: item.name }));
      }
    }
  } catch {
    // A missing index only costs the language picker; the page still works.
  }
  if (available.length === 0) available = [{ code: FALLBACK, name: 'English' }];

  const next =
    wanted && available.some((entry) => entry.code === wanted)
      ? wanted
      : languageFromPhone();

  const [preferred, fallback] = await Promise.all([
    fetchCatalog(next),
    next === FALLBACK ? Promise.resolve<Catalog>({}) : fetchCatalog(FALLBACK),
  ]);
  catalogs = { [next]: preferred, [FALLBACK]: next === FALLBACK ? preferred : fallback };
  current = next;
  return current;
}

/** The languages the page can be shown in, as the catalog folder lists them. */
export function languages(): LanguageEntry[] {
  return available;
}

export function language(): LanguageCode {
  return current;
}

/** Switch the interface language at runtime. */
export async function setLanguage(code: LanguageCode): Promise<LanguageCode> {
  const catalog = await fetchCatalog(code);
  catalogs[code] = catalog;
  current = code;
  return current;
}

/** Translate one key. Unknown keys come back as the key itself, never as empty. */
export function t(key: string): string {
  const catalog = catalogs[current];
  if (catalog && typeof catalog[key] === 'string') return catalog[key];
  const fallback = catalogs[FALLBACK];
  if (fallback && typeof fallback[key] === 'string') return fallback[key];
  return key;
}
