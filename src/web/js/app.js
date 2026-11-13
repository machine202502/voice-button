// Voice Button for the phone.
//
// The phone records, cuts the sound into small PCM16 chunks and sends them to
// the desktop, which transcribes them in the background. Every chunk is stored
// in IndexedDB before it is sent, so a lost connection never loses a word:
// when the link is back the chunks go out and the monolog shows the take with
// the time it was actually recorded.
//
// The pieces under this page are tested on their own:
//   storage.mjs  IndexedDB as two key/value bags
//   outbox.mjs   takes and their chunks, stored until the computer has them
//   sync.mjs     the only repeating work: send, fetch, draw
//   loop.mjs     the only timer, with backoff when the computer is away
//   stage.mjs    what every stage means and which buttons it offers
//   chat.mjs     the message list: oldest first, one divider per day

'use strict';

import { openStorage } from './storage.mjs';
import { createOutbox } from './outbox.mjs';
import { createSync } from './sync.mjs';
import { createLoop } from './loop.mjs';
import { describe } from './stage.mjs';
import { applyInstallEntry, installEntry } from './install.mjs';
import { backTarget, viewState } from './back.mjs';
import { buildFeed, dayKey, messageExpanded, recordingMessage, swipeDirection } from './chat.mjs';
import { atBottom } from './scroll.mjs';
import { SITE_VERSION } from './version.mjs';
import { Offline } from './offline.mjs';
import { bootOutcome, createCache } from './cache.mjs';

const API = '/site/api';
const RATE = 16000;
const CHUNK_SECONDS = 0.5;
const CHUNK_SAMPLES = Math.round(RATE * CHUNK_SECONDS);

// The page reads the feed and the list one page at a time, then keeps only the
// visible rows in the DOM. Heights are measured once a row has been on screen;
// the estimates below are only used for rows that have never been measured.
const LIST_PAGE = 100;
const FEED_PAGE = 50;
const EST_MONOLOG = 128;
const EST_MESSAGE = 104;
const EST_DAY = 40;
const EST_UNSENT = 104;
const EST_EMPTY = 80;
const EST_NEW_MONOLOG = 60;
const LOAD_MORE_PX = 700;
const OVERSCAN_MIN = 300;
// Messages kept in JS. Older pages are dropped from the far end while the
// reader moves up, newer ones while the reader moves down; the store keeps them
// all, so scrolling back reads the page again.
const FEED_WINDOW = 320;

// The token may arrive in the fragment when the page is moved between the
// public and the local address: fragments never reach the server.
if (location.hash.startsWith('#t=')) {
  const moved = decodeURIComponent(location.hash.slice(3));
  if (moved) localStorage.setItem('vb_token', moved);
  history.replaceState(null, '', location.pathname + location.search);
}

const app = {
  token: localStorage.getItem('vb_token') || '',
  lang: 'en',
  strings: {},
  outbox: null,
  sync: null,
  loop: null,
  cache: null,
  screen: 'list',
  monolog: null,
  lists: new Map(),
  listState: null,
  feed: null,
  archived: false,
  recording: null,
  installPrompt: null,
  installed: false,
  sheets: [],
  sheetId: 0,
  expandedMessage: null,
  swiping: false,
  recordingMonolog: null,
  listTop: 0,
  archiveTop: 0,
  reachable: true,
};

const el = (id) => document.getElementById(id);

// The on-screen keyboard covers the bottom of the layout viewport in Android
// browsers. Remember how much, so a sheet and its buttons sit above it.
function trackKeyboard() {
  const viewport = window.visualViewport;
  if (!viewport) return;
  const covered = Math.max(
    0,
    Math.round(window.innerHeight - viewport.height - viewport.offsetTop),
  );
  document.documentElement.style.setProperty('--keyboard', covered + 'px');
}
trackKeyboard();
if (window.visualViewport) {
  window.visualViewport.addEventListener('resize', trackKeyboard);
  window.visualViewport.addEventListener('scroll', trackKeyboard);
}

// The page's own version, right in the header: after a refresh it says whether
// the phone got the new page or is still showing a cached one.
{
  const version = el('version');
  if (version) version.textContent = 'v' + SITE_VERSION;
}

// Chrome decides installability once per load and fires this event early —
// before boot() finishes. Listening from the first line is the only way to
// keep the prompt.
window.addEventListener('beforeinstallprompt', (event) => {
  event.preventDefault();
  app.installPrompt = event;
  refreshInstallEntry();
});
window.addEventListener('appinstalled', () => {
  // The app is installed now: stop offering to install it.
  app.installed = true;
  app.installPrompt = null;
  refreshInstallEntry();
});

// ---------------------------------------------------------------- language

async function loadLanguage() {
  // The list comes from the lang folder itself, not from a language list in
  // this file: adding a catalog is enough to make it available.
  let available = ['en'];
  try {
    const index = await fetch('lang/index.json').then((response) => response.json());
    if (Array.isArray(index.languages) && index.languages.length > 0) {
      available = index.languages.map((item) => item.code);
    }
  } catch (error) {
    // The English fallback below still opens the page.
  }
  const preferred = (navigator.language || 'en').slice(0, 2).toLowerCase();
  app.lang = available.includes(preferred)
    ? preferred
    : available.includes('en')
      ? 'en'
      : available[0];
  try {
    app.strings = await fetch('lang/' + app.lang + '.json').then((response) => response.json());
  } catch (error) {
    app.strings = {};
  }
  document.documentElement.lang = app.lang;
  // The manifest is what the phone shows under the icon, so it follows the
  // page's language: one file per language, picked before the install prompt.
  const manifest = document.getElementById('manifest');
  if (manifest) manifest.href = 'manifest.' + app.lang + '.webmanifest';
  labelStatic();
}

/// Labels that live outside the drawn list: icons, the pair error, the version
/// badge and the recording hint. Keeping them here means index.html carries no
/// language.
function labelStatic() {
  el('back').setAttribute('aria-label', t('back'));
  el('archive').setAttribute('aria-label', t('archive'));
  el('menu').setAttribute('aria-label', t('monolog'));
  el('record').setAttribute('aria-label', t('record'));
  el('version').setAttribute('aria-label', t('site_version'));
  el('reload').textContent = t('reload');
}

function t(key, fallback) {
  return app.strings[key] || key;
}

/// Every catalog may provide one/few/many forms; Intl knows which form this
/// language needs, so no language is named here. A missing form falls back to
/// `many` for languages with a simpler plural rule.
function countLabel(count) {
  let form = 'many';
  try {
    const chosen = new Intl.PluralRules(app.lang).select(count);
    if (chosen === 'one' || chosen === 'few' || chosen === 'many') form = chosen;
  } catch (error) {
    // An unknown locale means the simple many form.
  }
  return count + ' ' + t('messages_' + form);
}

function cardDate(value) {
  if (!value) return '';
  return String(value).replace('T', ' ').slice(0, 16);
}

// ---------------------------------------------------------------- api

/// Remember whether the computer answers. The microphone's colour is a
/// promise about work actually happening; a dead connection must break it.
function setReachable(value) {
  if (app.reachable === value) return;
  app.reachable = value;
  updateComposer();
}

async function call(method, path, body) {
  const headers = {};
  if (app.token) headers.Authorization = 'Bearer ' + app.token;
  let payload;
  if (body instanceof ArrayBuffer) {
    payload = body;
    headers['Content-Type'] = 'application/octet-stream';
  } else if (body instanceof Uint8Array) {
    payload = body;
    headers['Content-Type'] = 'application/octet-stream';
  } else if (body !== undefined) {
    payload = JSON.stringify(body);
    headers['Content-Type'] = 'application/json';
  }
  let response;
  // A request to a computer that went away can hang for minutes; without a
  // deadline the whole sync loop stayed frozen and nothing ever resumed.
  const controller = new AbortController();
  const deadline = setTimeout(() => controller.abort(), 20000);
  try {
    response = await fetch(API + path, {
      method,
      headers,
      body: payload,
      signal: controller.signal,
    });
  } catch (error) {
    setReachable(false);
    throw new Offline(error.message);
  } finally {
    clearTimeout(deadline);
  }
  setReachable(true);
  if (response.status === 401) {
    showPairError();
    throw new Error('pairing required');
  }
  const text = await response.text();
  let json = {};
  try {
    json = text ? JSON.parse(text) : {};
  } catch (error) {
    json = {};
  }
  if (!response.ok || json.ok === false) {
    const refusal = new Error(json.error || 'HTTP ' + response.status);
    refusal.status = response.status;
    throw refusal;
  }
  return json;
}

/// Show a readable reason instead of a blank page. Every failure path ends
/// here: a phone has no console, so an empty screen is a dead end for the user.
function fail(text) {
  const page = el('app');
  const screen = el('pair-error');
  if (page) page.hidden = true;
  if (!screen) return;
  screen.hidden = false;
  const line = el('pair-text');
  if (line) line.textContent = text;
  const where = el('pair-where');
  if (where) where.textContent = location.origin;
}

function showPairError() {
  fail(t('pair_required'));
}

function reason(error) {
  if (error instanceof Offline) {
    return t('offline_long');
  }
  const message = String((error && error.message) || error || '');
  if (message.includes('local network')) {
    return t('lan_blocked');
  }
  if (message.includes('pairing required')) {
    return t('pair_required');
  }
  return t('cannot_reach') + (message ? ' · ' + message : '');
}

// ---------------------------------------------------------------- time

/// The phone owns the time: local wall clock with its own offset, so the
/// desktop never has to guess where the message came from.
function localIso() {
  const now = new Date();
  const pad = (value) => String(value).padStart(2, '0');
  const offset = -now.getTimezoneOffset();
  const sign = offset >= 0 ? '+' : '-';
  const abs = Math.abs(offset);
  return (
    now.getFullYear() +
    '-' + pad(now.getMonth() + 1) +
    '-' + pad(now.getDate()) +
    'T' + pad(now.getHours()) +
    ':' + pad(now.getMinutes()) +
    ':' + pad(now.getSeconds()) +
    sign + pad(Math.floor(abs / 60)) + ':' + pad(abs % 60)
  );
}

function shortTime(iso) {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return '';
  return String(date.getHours()).padStart(2, '0') + ':' + String(date.getMinutes()).padStart(2, '0');
}

function dayLabel(key) {
  const today = new Date();
  const localKey = (date) =>
    date.getFullYear() +
    '-' + String(date.getMonth() + 1).padStart(2, '0') +
    '-' + String(date.getDate()).padStart(2, '0');
  if (key === localKey(today)) return t('today');
  if (key === localKey(new Date(today.getTime() - 86400000))) return t('yesterday');
  const date = new Date(key + 'T00:00:00');
  if (Number.isNaN(date.getTime())) return key;
  return date.toLocaleDateString(app.lang, { weekday: 'short', day: '2-digit', month: '2-digit' });
}

// ---------------------------------------------------------------- views

function header(title, options = {}) {
  el('title').textContent = title;
  el('back').hidden = !options.back;
  el('archive').hidden = !!options.hideArchive || !!options.menu;
  el('menu').hidden = !options.menu;
  el('sub').textContent = options.sub || '';
  el('composer').hidden = !options.composer;
}

function button(label, className, onClick) {
  const node = document.createElement('button');
  node.textContent = label;
  if (className) node.className = className;
  node.addEventListener('click', onClick);
  return node;
}

/// A button that the one delegated click handler on #view understands.
function actionButton(label, className, act, data = {}) {
  const node = document.createElement('button');
  node.type = 'button';
  node.textContent = label;
  if (className) node.className = className;
  node.dataset.act = act;
  for (const [key, value] of Object.entries(data)) {
    if (value !== undefined && value !== null) node.dataset[key] = String(value);
  }
  return node;
}

function chip(label, kind) {
  const node = document.createElement('span');
  node.className = 'chip ' + (kind || '');
  node.textContent = label;
  return node;
}

function empty(text) {
  const node = document.createElement('div');
  node.className = 'empty';
  node.textContent = text;
  return node;
}

// ---------------------------------------------------------------- virtual list
//
// The feed and the monolog list are one scrolling column in #view. The page
// keeps an ordered row list (a page of messages, a page of monologs), measured
// heights for rows it has seen, and renders only rows in the visible window
// plus one screen of overscan. Two spacers stand in for everything that is not
// in the DOM, so the scrollbar still describes the real extent.

function padTop() {
  const value = parseFloat(getComputedStyle(el('view')).paddingTop);
  return Number.isFinite(value) ? value : 0;
}

function activeState() {
  if (app.screen === 'feed' && app.feed) return app.feed;
  if (app.screen === 'list' && app.listState) return app.listState;
  return null;
}

function rowHeight(state, row) {
  const saved = state.heights.get(row.key);
  if (saved && saved.sig === row.sig) return saved.height;
  return row.estimate;
}

function layoutRows(state) {
  const rows = state.rows;
  const offsets = new Array(rows.length);
  const heights = new Array(rows.length);
  let total = state.extraTop || 0;
  for (let index = 0; index < rows.length; index += 1) {
    offsets[index] = total;
    heights[index] = rowHeight(state, rows[index]);
    total += heights[index];
  }
  return { offsets, heights, total };
}

function visibleRows(state, layout, scrollTop, viewport) {
  const rows = state.rows;
  if (rows.length === 0) return { start: 0, end: 0 };
  const overscan = Math.max(OVERSCAN_MIN, viewport);
  const from = Math.max(0, scrollTop - overscan);
  const to = scrollTop + viewport + overscan;
  let start = 0;
  while (
    start < rows.length &&
    layout.offsets[start] + layout.heights[start] < from
  ) {
    start += 1;
  }
  let end = start;
  while (end < rows.length && layout.offsets[end] < to) end += 1;
  if (end === start && start < rows.length) end = start + 1;
  return { start, end };
}

function spacer(height) {
  const node = document.createElement('div');
  node.className = 'vspacer';
  node.setAttribute('aria-hidden', 'true');
  node.style.height = Math.max(0, Math.round(height)) + 'px';
  return node;
}

function virtualRow(state, row, index) {
  const node = document.createElement('div');
  node.className = 'vrow';
  node.dataset.index = String(index);
  node.dataset.key = row.key;
  node.append(row.build());
  return node;
}

/// Where the first visible row sits, so a repaint (or older messages arriving
/// above it) can keep that row exactly where the reader had it.
function captureAnchor(state, layout) {
  if (state.scrollToBottom) return null;
  const view = el('view');
  const pad = padTop();
  const top = Math.max(0, view.scrollTop - pad);
  for (let index = 0; index < state.rows.length; index += 1) {
    if (layout.offsets[index] + layout.heights[index] <= top) continue;
    return { key: state.rows[index].key, distance: layout.offsets[index] - top };
  }
  return null;
}

/// Remember the reader's place before the rows change. Capturing after a page
/// was prepended would anchor to the new top row and lose the position.
function rememberAnchor(state) {
  state.anchor = state.scrollToBottom ? null : captureAnchor(state, layoutRows(state));
}

function schedulePaint(state = activeState()) {
  if (!state || state.painting || state.paintQueued) return;
  state.paintQueued = true;
  requestAnimationFrame(() => {
    state.paintQueued = false;
    if (state === activeState()) paintVirtual(state);
  });
}

function paintVirtual(state) {
  if (!state || state !== activeState() || state.painting) return;
  if (app.swiping) return;
  const view = el('view');
  state.painting = true;
  let again = false;
  try {
    const pad = padTop();
    const viewport = view.clientHeight || 600;
    const oldScroll = Math.max(0, view.scrollTop - pad);
    const oldLayout = layoutRows(state);
    const anchor =
      state.anchor !== undefined
        ? state.anchor
        : captureAnchor(state, oldLayout);
    state.anchor = undefined;
    const scrollToBottom = state.scrollToBottom === true;
    state.scrollToBottom = false;
    const maxScroll = Math.max(0, oldLayout.total - viewport);
    const scrollTop = scrollToBottom ? maxScroll : Math.min(oldScroll, maxScroll);
    const windowRange = visibleRows(state, oldLayout, scrollTop, viewport);

    const fragment = document.createDocumentFragment();
    const topHeight = oldLayout.offsets[windowRange.start] || 0;
    if (topHeight > 0.5) fragment.append(spacer(topHeight));
    for (let index = windowRange.start; index < windowRange.end; index += 1) {
      fragment.append(virtualRow(state, state.rows[index], index));
    }
    const last = windowRange.end - 1;
    const bottomHeight =
      last >= windowRange.start
        ? oldLayout.total - (oldLayout.offsets[last] + oldLayout.heights[last])
        : Math.max(0, oldLayout.total - topHeight);
    if (bottomHeight > 0.5) fragment.append(spacer(bottomHeight));

    const previousScrollTop = view.scrollTop;
    view.replaceChildren(fragment);
    view.scrollTop = previousScrollTop;

    // Measure what really landed on screen. Rows above and below keep their
    // estimate until they are seen; only measured rows contribute an exact
    // height to the scroll extent.
    let measured = false;
    for (const node of view.querySelectorAll('.vrow')) {
      const index = Number(node.dataset.index);
      const row = state.rows[index];
      if (!row) continue;
      const height = Math.max(0, node.getBoundingClientRect().height);
      const saved = state.heights.get(row.key);
      if (!saved || saved.sig !== row.sig || Math.abs(saved.height - height) > 0.5) {
        state.heights.set(row.key, { height, sig: row.sig });
        measured = true;
      }
    }

    const nextLayout = measured ? layoutRows(state) : oldLayout;
    if (scrollToBottom) {
      view.scrollTop = Math.max(0, view.scrollHeight - view.clientHeight);
    } else if (anchor) {
      const index = state.rows.findIndex((row) => row.key === anchor.key);
      view.scrollTop =
        index >= 0
          ? Math.max(0, pad + nextLayout.offsets[index] - anchor.distance)
          : Math.max(0, pad + scrollTop);
    } else {
      view.scrollTop = Math.max(0, pad + scrollTop);
    }
    state.window = windowRange;
    state.painted = true;
    const visible = visibleRows(
      state,
      nextLayout,
      Math.max(0, view.scrollTop - pad),
      viewport,
    );
    again =
      measured &&
      (visible.start < windowRange.start || visible.end > windowRange.end);
  } finally {
    state.painting = false;
    if (again) schedulePaint(state);
  }
}

function mountScreen() {
  const view = el('view');
  el('app').hidden = false;
  el('pair-error').hidden = true;
  view.classList.add('virtual');
  view.replaceChildren();
  view.scrollTop = 0;
}

/// The window of rows that a given scroll offset covers. Cheap enough to run on
/// every scroll event, which is the point: see `onViewScroll`.
function windowAt(state, scrollTop, viewport) {
  return visibleRows(state, layoutRows(state), scrollTop, viewport);
}

function sameWindow(left, right) {
  return left.start === right.start && left.end === right.end;
}

function onViewScroll() {
  const state = activeState();
  if (!state || state.painting) return;
  if (state.kind === 'feed') onFeedScroll(state);
  else onListScroll(state);
  // Repainting the whole list on every scroll event made the page flicker while
  // a flick was decelerating: the window was torn down and rebuilt many times a
  // second, and the scroll offset was restored after each rebuild. A paint is
  // only needed when the set of visible rows actually changes; scrolling inside
  // the same window needs none.
  const view = el('view');
  const viewport = view.clientHeight || 600;
  const next = windowAt(state, Math.max(0, view.scrollTop - padTop()), viewport);
  if (sameWindow(next, state.window)) return;
  schedulePaint(state);
}

el('view').addEventListener('scroll', onViewScroll, { passive: true });
window.addEventListener('resize', () => schedulePaint());

// ---------------------------------------------------------------- monolog list

function createListState(archived) {
  return {
    kind: 'list',
    archived: Boolean(archived),
    records: [],
    pending: [],
    total: 0,
    loading: false,
    heights: new Map(),
    rows: [],
    extraTop: 0,
    window: { start: 0, end: 0 },
    painting: false,
    paintQueued: false,
    scrollToBottom: false,
    painted: false,
  };
}

function monologSignature(monolog, waiting, recordingHere) {
  return [
    monolog.id,
    monolog.name,
    monolog.lastAt || monolog.created || '',
    monolog.last || '',
    monolog.count || 0,
    monolog.local ? 1 : 0,
    monolog.busy ? 1 : 0,
    waiting,
    recordingHere ? 1 : 0,
  ].join('|');
}

function buildListRows(state) {
  const rows = state.records.map((monolog) => {
    const waiting = state.pending.filter(
      (take) => Number(take.monolog) === Number(monolog.id) && !take.remote,
    ).length;
    // A take recorded right now belongs to this card too, and the card has to
    // be repainted when it starts or stops.
    const recordingHere = Boolean(
      app.recording && Number(app.recording.take.monolog) === Number(monolog.id),
    );
    return {
      key: 'monolog:' + monolog.id,
      sig: monologSignature(monolog, waiting, recordingHere),
      estimate: EST_MONOLOG,
      build: () => monologRowNode(monolog, waiting, recordingHere),
    };
  });
  if (state.records.length === 0) {
    rows.push({
      key: 'empty',
      sig: 'empty:' + (state.archived ? 1 : 0),
      estimate: EST_EMPTY,
      build: () => empty(state.archived ? t('archive_empty') : t('no_monologs')),
    });
  }
  if (!state.archived && state.records.length >= state.total) {
    rows.push({
      key: 'new-monolog',
      sig: 'new-monolog',
      estimate: EST_NEW_MONOLOG,
      build: () => {
        const create = document.createElement('button');
        create.className = 'new-monolog';
        create.textContent = t('new_monolog');
        create.dataset.act = 'new-monolog';
        return create;
      },
    });
  }
  return rows;
}

function monologRowNode(monolog, waiting, recordingHere = false) {
  const card = document.createElement('div');
  card.className = 'monolog-card';
  card.setAttribute('role', 'button');
  card.tabIndex = 0;
  card.dataset.card = 'monolog';
  card.dataset.id = String(monolog.id);
  card.innerHTML =
    '<div class="top"><strong></strong><span class="when"></span>' +
    '<button class="more" type="button" aria-label="⋯" data-act="monolog-menu"></button></div>' +
    '<div class="last"></div><div class="count"></div>';
  card.querySelector('.more').textContent = '⋯';
  card.querySelector('.more').dataset.id = String(monolog.id);
  card.querySelector('strong').textContent = monolog.name;
  if (monolog.local) {
    card.querySelector('.top').append(chip(t('waiting_link'), 'unsent'));
  }
  card.querySelector('.when').textContent = cardDate(monolog.lastAt || monolog.created);
  card.querySelector('.last').textContent = monolog.last || '';
  // Takes that never reached the computer are not in its count yet, so the
  // card has to add them: an offline monolog used to show "0 messages" right
  // after the user dictated into it.
  card.querySelector('.count').textContent = countLabel(
    monolog.local ? waiting : (monolog.count || 0) + waiting,
  );
  // Unfinished work is visible from the list, in two different colours, because
  // the reason matters: the computer is working on it (lime, and this card is
  // the one being recorded into), or the take is still on the phone waiting for
  // a connection (amber).
  const working = monolog.busy || recordingHere;
  if (working) card.classList.add('active');
  if (waiting > 0) card.classList.add('unsent');
  if (working) card.title = t('card_working');
  else if (waiting > 0) card.title = t('card_unsent');
  const row = document.createElement('div');
  row.className = 'swipe-row';
  const action = document.createElement('div');
  action.className = 'swipe-action' + (app.archived ? ' out' : '');
  action.textContent = app.archived ? t('unarchive') : t('archive');
  row.append(action, card);
  if (!monolog.local) attachSwipe(row, card, action, monolog);
  return row;
}

function appendListRecords(state, list) {
  const seen = new Set(state.records.map((record) => Number(record.id)));
  let added = 0;
  for (const record of list) {
    const id = Number(record.id);
    if (seen.has(id)) continue;
    seen.add(id);
    state.records.push(record);
    added += 1;
  }
  return added;
}

async function refreshListState(state) {
  rememberAnchor(state);
  const count = Math.max(LIST_PAGE, state.records.length);
  const page = await app.cache.monologPage(state.archived, 0, count);
  state.records = page.monologs;
  const localCount = state.archived ? 0 : (await app.cache.localMonologs()).length;
  const serverTotal = app.sync ? app.sync.monologTotal(state.archived) : null;
  state.total = Math.max(
    page.total,
    serverTotal == null ? 0 : serverTotal + localCount,
    state.records.length,
  );
  state.rows = buildListRows(state);
}

async function loadMoreList(state) {
  if (state.loading || state.records.length >= state.total) return;
  state.loading = true;
  rememberAnchor(state);
  try {
    let page = await app.cache.monologPage(
      state.archived,
      state.records.length,
      LIST_PAGE,
    );
    let added = appendListRecords(state, page.monologs);
    if (added === 0 && state.records.length < state.total && app.sync) {
      const cachedServer = await app.cache.serverMonologCount(state.archived);
      const answer = await app.sync.fetchMonologPage(
        state.archived,
        cachedServer,
        LIST_PAGE,
      );
      if (answer) {
        const localCount = state.archived
          ? 0
          : (await app.cache.localMonologs()).length;
        if (answer.total != null) {
          state.total = Math.max(state.total, answer.total + localCount);
        }
        page = await app.cache.monologPage(
          state.archived,
          state.records.length,
          LIST_PAGE,
        );
        added = appendListRecords(state, page.monologs);
        if (answer.hasMore === false && added === 0) state.total = state.records.length;
      } else {
        // Offline: whatever is stored is the whole list there is.
        state.total = state.records.length;
      }
    }
    if (added === 0 && page.total <= state.records.length) {
      state.total = state.records.length;
    }
    state.rows = buildListRows(state);
    paintVirtual(state);
  } finally {
    state.loading = false;
  }
}

function onListScroll(state) {
  const layout = layoutRows(state);
  const view = el('view');
  const pad = padTop();
  const below = layout.total + pad - (view.scrollTop + view.clientHeight);
  if (below < LOAD_MORE_PX && state.records.length < state.total && !state.loading) {
    loadMoreList(state).catch(() => {});
  }
}

function findMonolog(id) {
  const state = app.listState;
  if (state) {
    const found = state.records.find((monolog) => Number(monolog.id) === Number(id));
    if (found) return found;
  }
  if (app.monolog && Number(app.monolog.id) === Number(id)) return app.monolog;
  return null;
}

/// One listener for every row ever rendered. Virtual rows come and go with the
/// scroll window, so per-node listeners would be lost on every repaint.
function onViewClick(event) {
  const target = event.target && event.target.closest
    ? event.target
    : null;
  if (!target) return;
  const action = target.closest('[data-act]');
  if (action) {
    const act = action.dataset.act;
    if (act === 'monolog-menu') {
      const monolog = findMonolog(action.dataset.id);
      if (monolog) monologMenuSheet(monolog);
      return;
    }
    if (act === 'new-monolog') {
      createMonologSheet();
      return;
    }
    const feed = app.feed;
    if (!feed) return;
    if (act === 'discard') {
      const take = feed.pending.find(
        (item) => String(item.localId) === String(action.dataset.local),
      );
      if (take) discardTake(take);
      return;
    }
    const message = messageInFeed(action.dataset.id);
    if (!message) return;
    if (act === 'expand') {
      app.expandedMessage =
        Number(app.expandedMessage) === Number(message.id) ? null : Number(message.id);
      rememberAnchor(feed);
      rebuildFeedRows(feed);
      paintVirtual(feed);
    } else if (act === 'copy') {
      copy(message.text);
    } else if (act === 'edit') {
      editSheet(message);
    } else if (act === 'retry') {
      retryMessage(message);
    } else if (act === 'delete') {
      deleteSheet(message);
    }
    return;
  }
  const card = target.closest('[data-card="monolog"]');
  if (card) {
    const monolog = findMonolog(card.dataset.id);
    if (monolog) openMonolog(monolog);
  }
}

el('view').addEventListener('click', onViewClick);

function attachSwipe(row, card, action, monolog) {
  let startX = 0;
  let startY = 0;
  let dx = 0;
  let dy = 0;
  let axis = null;
  let active = false;
  let suppressClick = false;

  const reset = () => {
    app.swiping = false;
    row.classList.remove('dragging', 'leaving');
    card.style.transform = '';
    card.style.opacity = '';
    action.style.opacity = '';
  };

  card.addEventListener('pointerdown', (event) => {
    if (event.target.closest('.more')) return;
    if (event.pointerType === 'mouse' && event.button !== 0) return;
    if (app.recording && app.recording.take && app.recording.take.monolog === monolog.id) return;
    active = true;
    app.swiping = true;
    axis = null;
    dx = 0;
    dy = 0;
    startX = event.clientX;
    startY = event.clientY;
    suppressClick = false;
    row.classList.add('dragging');
    try {
      card.setPointerCapture(event.pointerId);
    } catch {
      // Synthetic pointers in tests cannot be captured; the swipe still works.
    }
  });

  card.addEventListener('pointermove', (event) => {
    if (!active) return;
    dx = event.clientX - startX;
    dy = event.clientY - startY;
    if (!axis) {
      if (Math.abs(dx) < 10 && Math.abs(dy) < 10) return;
      axis = Math.abs(dx) > Math.abs(dy) ? 'x' : 'y';
      if (axis === 'y') {
        reset();
        return;
      }
    }
    if (axis !== 'x') return;
    if (event.cancelable) event.preventDefault();
    card.style.transform = 'translate3d(' + dx + 'px,0,0)';
    card.style.opacity = String(Math.max(0.35, 1 - Math.abs(dx) / 420));
    action.style.opacity = String(Math.min(1, Math.abs(dx) / 80));
    action.style.justifyContent = dx < 0 ? 'flex-end' : 'flex-start';
  });

  const finish = () => {
    if (!active) return;
    active = false;
    const direction = swipeDirection(dx, dy);
    if (!direction) {
      reset();
      return;
    }
    suppressClick = true;
    app.swiping = false;
    row.classList.remove('dragging');
    row.classList.add('leaving');
    const sign = dx < 0 ? -1 : 1;
    card.style.transform = 'translate3d(' + sign * 120 + '%,0,0)';
    card.style.opacity = '0';
    window.setTimeout(async () => {
      const archived = !app.archived;
      try {
        await call('POST', '/monologues/' + monolog.id + '/archive', { archived });
        await app.cache.setArchived(monolog.id, archived);
        await renderMonologs();
        toast(archived ? t('archive') : t('unarchive'));
      } catch (error) {
        toast(t('offline'));
        await renderMonologs();
      }
    }, 190);
  };

  card.addEventListener('pointerup', finish);
  card.addEventListener('pointercancel', finish);
  card.addEventListener(
    'click',
    (event) => {
      if (!suppressClick) return;
      event.preventDefault();
      event.stopImmediatePropagation();
      suppressClick = false;
    },
    true,
  );
}

/// Draw the list from what is stored. The refresh from the computer is the sync
/// loop's work, so this never fails on a dead connection.
async function renderMonologs(top = undefined) {
  if (app.swiping) return;
  const wasList =
    app.screen === 'list' &&
    app.listState &&
    app.listState.archived === app.archived;
  const keep = wasList && top === undefined ? el('view').scrollTop : null;
  const key = app.archived ? '1' : '0';
  app.monolog = null;
  app.screen = 'list';
  app.feed = null;
  app.sync.show(0);
  app.sync.archive(app.archived);
  header(app.archived ? t('archive') : t('monologs'), {
    hideArchive: app.archived,
  });
  updateComposer();
  let state = app.lists.get(key);
  if (!state) {
    state = createListState(app.archived);
    app.lists.set(key, state);
    const page = await app.cache.monologPage(app.archived, 0, LIST_PAGE);
    state.records = page.monologs;
    const localCount = app.archived ? 0 : (await app.cache.localMonologs()).length;
    const serverTotal = app.sync.monologTotal(app.archived);
    state.total = Math.max(
      page.total,
      serverTotal == null ? 0 : serverTotal + localCount,
      state.records.length,
    );
  } else {
    await refreshListState(state);
  }
  state.pending = await app.outbox.list();
  state.rows = buildListRows(state);
  app.listState = state;
  mountScreen();
  paintVirtual(state);
  const target =
    top !== undefined
      ? top
      : keep !== null
        ? keep
        : app.archived
          ? app.archiveTop
          : app.listTop;
  if (target > 0) {
    // Coming back into the middle of a long list: page until the saved offset
    // is inside the list, one page at a time.
    const view = el('view');
    let guard = 0;
    while (
      guard < 20 &&
      state.records.length < state.total &&
      layoutRows(state).total + padTop() < target + view.clientHeight
    ) {
      const before = state.records.length;
      await loadMoreList(state);
      guard += 1;
      if (state.records.length === before) break;
    }
  }
  el('view').scrollTop = Math.max(0, Number(target) || 0);
  schedulePaint(state);
  // The loop is the only timer: ask it to refresh instead of fetching here.
  if (app.loop) app.loop.kick();
}

async function openMonolog(monolog) {
  const state = history.state || {};
  if (state.view !== 'monolog' || state.id !== monolog.id) {
    pushView(viewState({ monolog }));
  }
  // Remember where the list was: coming back from a chat must not drop the
  // reader at the bottom of a long list.
  if (app.archived) app.archiveTop = el('view').scrollTop;
  else app.listTop = el('view').scrollTop;
  app.monolog = { id: monolog.id, name: monolog.name, local: !!monolog.local };
  // A freshly opened chat starts at the newest message; after that the reader
  // owns the scroll position.
  app.screen = 'feed';
  app.sync.show(monolog.id);
  // An archived monolog can be read and pulled out of the archive, never
  // changed or recorded into.
  header(monolog.name, { back: true, composer: !app.archived, menu: true });
  updateComposer();
  mountScreen();
  const feed = createFeedState(app.monolog);
  app.feed = feed;
  // Cached pages and the waiting takes are on the screen before the computer
  // is asked, so opening a monolog offline is never a blank wait.
  feed.pending = (await app.outbox.list()).filter(
    (take) => Number(take.monolog) === Number(monolog.id) && !take.remote,
  );
  const page = await app.cache.messagePage(monolog.id, { limit: FEED_PAGE });
  feed.loaded = dedupeMessages(page.messages);
  feed.byId = new Map(feed.loaded.map((message) => [Number(message.id), message]));
  feed.total = Math.max(feed.loaded.length, await app.cache.messageCount(monolog.id));
  feed.hasOlder = feed.loaded.length < feed.total || page.hasMore === true;
  feed.scrollToBottom = true;
  rebuildFeedRows(feed);
  paintVirtual(feed);
  try {
    await app.sync.tick();
  } catch (error) {
    if (error instanceof Offline) offlineNote();
    else fail(reason(error));
  }
}

// ---------------------------------------------------------------- feed
//
// The feed keeps a contiguous, oldest-first slice of the monolog: the newest
// page on open, older pages prepended while the reader scrolls up. The rows in
// between are day separators and message cards; the page adds one height per
// measured row and a spacer for the older messages that are not loaded yet.

function createFeedState(monolog) {
  return {
    kind: 'feed',
    monolog: { ...monolog },
    loaded: [],
    byId: new Map(),
    pending: [],
    total: 0,
    hasOlder: false,
    atNewest: true,
    loadingOlder: false,
    loadingNewer: false,
    heights: new Map(),
    rows: [],
    extraTop: 0,
    window: { start: 0, end: 0 },
    painting: false,
    paintQueued: false,
    scrollToBottom: false,
    painted: false,
  };
}

function contentSignature(message) {
  return [
    message.id,
    message.recorded || '',
    message.stage || '',
    message.text || '',
    message.edited ? 1 : 0,
    message.waiting ? 1 : 0,
  ].join('|');
}

function rowSignature(message) {
  return [
    contentSignature(message),
    app.expandedMessage === message.id ? 1 : 0,
    recordingMessage(app.recording, message.id) ? 1 : 0,
    app.archived ? 1 : 0,
  ].join('|');
}

function takeSignature(take) {
  return [
    take.localId,
    take.remote ?? '',
    take.recorded || '',
    take.closed ? 1 : 0,
    take.monolog ?? '',
  ].join('|');
}

function compareMessages(left, right) {
  const a = String(left.recorded || '');
  const b = String(right.recorded || '');
  if (a < b) return -1;
  if (a > b) return 1;
  return Number(left.id) - Number(right.id);
}

function dedupeMessages(list) {
  const seen = new Set();
  const found = [];
  for (const message of list || []) {
    const id = Number(message.id);
    if (!Number.isFinite(id) || seen.has(id)) continue;
    seen.add(id);
    found.push(message);
  }
  found.sort(compareMessages);
  return found;
}

function insertMessage(feed, message) {
  const index = feed.loaded.findIndex((other) => compareMessages(other, message) > 0);
  if (index < 0) feed.loaded.push(message);
  else feed.loaded.splice(index, 0, message);
}

function messageEstimate(message) {
  if (!message.text) return 96;
  if (message.text.length > 320) {
    const expanded = messageExpanded(app.recording, message.id, app.expandedMessage);
    return expanded ? 320 : 132;
  }
  return EST_MESSAGE;
}

function rebuildFeedRows(feed) {
  const rows = [];
  for (const row of buildFeed(feed.loaded)) {
    if (row.kind === 'day') {
      rows.push({
        key: 'day:' + row.key,
        sig: 'day:' + row.key,
        estimate: EST_DAY,
        build: () => {
          const separator = document.createElement('div');
          separator.className = 'day';
          separator.textContent = dayLabel(row.key);
          return separator;
        },
      });
    } else {
      const message = row.message;
      rows.push({
        key: 'msg:' + message.id,
        sig: rowSignature(message),
        estimate: messageEstimate(message),
        build: () => messageCard(message),
      });
    }
  }
  for (const take of feed.pending) {
    rows.push({
      key: 'take:' + take.localId,
      sig: takeSignature(take),
      estimate: EST_UNSENT,
      build: () => unsentCard(take),
    });
  }
  if (rows.length === 0) {
    rows.push({
      key: 'empty',
      sig: 'empty',
      estimate: EST_EMPTY,
      build: () => empty(t('no_messages')),
    });
  }
  feed.rows = rows;
  // Only loaded rows contribute to the scroll extent. Older pages are fetched
  // when the reader reaches the top; the scroll position is anchored to the
  // first visible row, so prepending a page never moves what is being read.
  feed.extraTop = 0;
}

function samePending(left, right) {
  if (left.length !== right.length) return false;
  for (let index = 0; index < left.length; index += 1) {
    if (takeSignature(left[index]) !== takeSignature(right[index])) return false;
  }
  return true;
}

/// Drop the messages that are farther from the reader than the window. The
/// records are still on disk; the cursor for the next page is the remaining
/// tail, so nothing is lost.
function dropFeedMessages(feed, count, fromStart) {
  if (count <= 0) return;
  const removed = fromStart
    ? feed.loaded.splice(0, count)
    : feed.loaded.splice(feed.loaded.length - count, count);
  for (const message of removed) feed.byId.delete(Number(message.id));
}

function pruneFeedHeights(feed) {
  const keys = new Set(feed.rows.map((row) => row.key));
  for (const key of [...feed.heights.keys()]) {
    if (!keys.has(key)) feed.heights.delete(key);
  }
}

/// The reader moved up: keep the older end, drop the newest messages from
/// memory. They are fetched again with `after` on the way down.
function trimFeedTail(feed) {
  if (feed.loaded.length <= FEED_WINDOW) return false;
  dropFeedMessages(feed, feed.loaded.length - FEED_WINDOW, false);
  feed.atNewest = false;
  return true;
}

/// The reader moved down: keep the newer end, drop the oldest messages from
/// memory. They are fetched again with `before` on the way up.
function trimFeedHead(feed) {
  if (feed.loaded.length <= FEED_WINDOW) return false;
  dropFeedMessages(feed, feed.loaded.length - FEED_WINDOW, true);
  feed.hasOlder = true;
  return true;
}

/// Draw the newest page the sync loop fetched. A redraw keeps the row the
/// reader is looking at where it is; only a reader already at the bottom follows
/// new messages.
function draw(page, pending) {
  const feed = app.feed;
  if (!feed || !app.monolog) return;
  if (page && page.monolog != null && Number(page.monolog) !== Number(feed.monolog.id)) {
    return;
  }
  const view = el('view');
  const wasAtBottom = atBottom({
    offset: view.scrollTop,
    height: view.scrollHeight,
    viewport: view.clientHeight,
  });
  const messages = page && Array.isArray(page.messages) ? page.messages : [];
  let changed = false;
  rememberAnchor(feed);

  const nextPending = (pending || []).filter(
    (take) => Number(take.monolog) === Number(feed.monolog.id) && !take.remote,
  );
  if (!samePending(feed.pending, nextPending)) {
    feed.pending = nextPending;
    changed = true;
  }

  if (page && page.total != null) {
    const total = Number(page.total);
    if (Number.isFinite(total) && total !== feed.total) {
      feed.total = Math.max(total, feed.loaded.length);
      changed = true;
    }
  }

  const lastLoaded = feed.loaded[feed.loaded.length - 1] || null;
  // The reader is in an older window and the newest page starts after its
  // tail: applying it would drag the window forward. The page is fetched again
  // when the reader comes back down.
  const blocked =
    feed.atNewest === false &&
    lastLoaded !== null &&
    messages.length > 0 &&
    compareMessages(messages[0], lastLoaded) > 0;
  if (!blocked) {
    for (const message of messages) {
      const id = Number(message.id);
      if (!Number.isFinite(id)) continue;
      const previous = feed.byId.get(id);
      if (previous) {
        if (contentSignature(previous) !== contentSignature(message)) {
          feed.byId.set(id, message);
          const index = feed.loaded.findIndex((item) => Number(item.id) === id);
          if (index >= 0) feed.loaded[index] = message;
          changed = true;
        }
        continue;
      }
      feed.byId.set(id, message);
      insertMessage(feed, message);
      changed = true;
    }
  }
  // The newest page did not touch the loaded tail: more than one page arrived
  // since the last tick. Fill the gap with `after` pages instead of pretending
  // the feed is contiguous.
  const overlap =
    lastLoaded && messages.length > 0 && compareMessages(messages[0], lastLoaded) <= 0;
  const gap = Boolean(
    !blocked && lastLoaded && messages.length >= FEED_PAGE && !overlap,
  );

  if (!changed) {
    feed.anchor = undefined;
    if (gap) fillNewer(feed).catch(() => {});
    return;
  }
  feed.total = Math.max(feed.total, feed.loaded.length);
  if (page && page.hasMore === true) feed.hasOlder = true;
  if (!blocked && (messages.length === 0 || !lastLoaded || messages.length < FEED_PAGE || overlap)) {
    feed.atNewest = true;
  }
  if (feed.loaded.length > FEED_WINDOW) {
    if (wasAtBottom) trimFeedHead(feed);
    else trimFeedTail(feed);
  }
  rebuildFeedRows(feed);
  pruneFeedHeights(feed);
  if (!feed.painted || wasAtBottom) feed.scrollToBottom = true;
  paintVirtual(feed);
  updateComposer();
  if (gap) fillNewer(feed).catch(() => {});
}

/// One older page, IndexedDB first. Prepending a page changes the content above
/// the reader; `paintVirtual` anchors the first visible row so the screen does
/// not jump.
async function loadOlder(feed) {
  if (feed.loadingOlder || !feed.hasOlder) return;
  feed.loadingOlder = true;
  try {
    const first = feed.loaded[0] || null;
    const cursor = first ? { recorded: first.recorded, id: first.id } : null;
    const page = await app.sync.page(feed.monolog.id, {
      before: cursor,
      limit: FEED_PAGE,
    });
    if (feed !== app.feed) return;
    if (page.total != null) {
      const total = Number(page.total);
      if (Number.isFinite(total)) feed.total = Math.max(feed.total, total);
    }
    rememberAnchor(feed);
    let added = 0;
    for (const message of page.messages) {
      const id = Number(message.id);
      if (!Number.isFinite(id) || feed.byId.has(id)) continue;
      feed.byId.set(id, message);
      feed.loaded.push(message);
      added += 1;
    }
    if (added > 0) {
      feed.loaded.sort(compareMessages);
      feed.hasOlder =
        page.hasMore === true || page.messages.length >= FEED_PAGE;
      trimFeedTail(feed);
      rebuildFeedRows(feed);
      pruneFeedHeights(feed);
      paintVirtual(feed);
    } else {
      feed.anchor = undefined;
      feed.hasOlder = page.hasMore === true;
    }
  } finally {
    feed.loadingOlder = false;
  }
}

/// A page newer than the loaded tail, for the moments the in-memory window was
/// trimmed or more than one page arrived between ticks. The normal newest page
/// is the one `tick` fetches.
async function fillNewer(feed) {
  if (feed.loadingNewer || feed.atNewest) return;
  feed.loadingNewer = true;
  try {
    let guard = 0;
    while (guard < 20 && feed === app.feed) {
      guard += 1;
      const last = feed.loaded[feed.loaded.length - 1];
      if (!last) break;
      const page = await app.sync.page(feed.monolog.id, {
        after: { recorded: last.recorded, id: last.id },
        limit: FEED_PAGE,
      });
      if (feed !== app.feed) return;
      if (page.messages.length === 0) {
        feed.atNewest = true;
        break;
      }
      rememberAnchor(feed);
      let added = 0;
      for (const message of page.messages) {
        const id = Number(message.id);
        if (!Number.isFinite(id) || feed.byId.has(id)) continue;
        feed.byId.set(id, message);
        feed.loaded.push(message);
        added += 1;
      }
      if (added > 0) {
        feed.loaded.sort(compareMessages);
        trimFeedHead(feed);
        rebuildFeedRows(feed);
        pruneFeedHeights(feed);
        paintVirtual(feed);
      } else {
        feed.anchor = undefined;
      }
      if (page.messages.length < FEED_PAGE) {
        feed.atNewest = true;
        break;
      }
    }
  } finally {
    feed.loadingNewer = false;
  }
}

function onFeedScroll(feed) {
  const view = el('view');
  const pad = padTop();
  const layout = layoutRows(feed);
  const top = Math.max(0, view.scrollTop - pad);
  if (top < LOAD_MORE_PX && feed.hasOlder && !feed.loadingOlder) {
    loadOlder(feed).catch(() => {});
  }
  const below = layout.total + pad - (view.scrollTop + view.clientHeight);
  if (below < 320 && !feed.atNewest && !feed.loadingNewer) {
    fillNewer(feed).catch(() => {});
  }
}

function removeFeedMessage(id) {
  const feed = app.feed;
  if (!feed) return;
  const number = Number(id);
  if (!feed.byId.has(number)) return;
  rememberAnchor(feed);
  feed.byId.delete(number);
  feed.loaded = feed.loaded.filter((message) => Number(message.id) !== number);
  feed.total = Math.max(feed.loaded.length, feed.total - 1);
  feed.hasOlder = feed.loaded.length < feed.total;
  rebuildFeedRows(feed);
  paintVirtual(feed);
}

function messageInFeed(id) {
  const feed = app.feed;
  return feed ? feed.byId.get(Number(id)) || null : null;
}

function messageCard(message) {
  const info = describe(message.stage);
  // The computer may be busy with the person in front of it: the take is not
  // lost, it waits. Saying so is the difference between a bug and a queue.
  const waiting = message.waiting === true;
  const card = document.createElement('article');
  card.className = 'message';
  if (info.busy) card.classList.add('live');
  if (info.tone === 'warning') card.classList.add('failed');

  const time = document.createElement('div');
  time.className = 'time';
  time.append(document.createTextNode(shortTime(message.recorded)));
  if (info.stage !== 'done') time.append(chip(t(info.label, info.label), info.tone === 'live' ? 'live' : info.tone === 'warning' ? 'failed' : ''));
  if (waiting) time.append(chip(t('engine_busy'), 'busy'));
  if (message.edited) time.append(chip(t('edited'), 'edited'));
  card.append(time);

  const long = Boolean(message.text) && message.text.length > 320;
  // While the microphone is open, the card it is writing into is always fully
  // expanded: the person must see every word of the take in progress. Other
  // cards stay closed, and the recording card cannot be collapsed.
  const inRecording = recordingMessage(app.recording, message.id);
  const expanded = messageExpanded(app.recording, message.id, app.expandedMessage);
  const text = document.createElement('div');
  text.className = 'text' + (message.text ? '' : ' empty');
  const hint = message.text
    ? ''
    : waiting
      ? t('busy_hint')
      : info.hint
        ? t(info.hint, '')
        : '';
  text.textContent = message.text
    ? long && !expanded
      ? message.text.slice(0, 240).trimEnd() + '…'
      : message.text
    : hint;
  card.append(text);
  if (long && !app.recording && !inRecording) {
    card.append(
      actionButton(expanded ? t('collapse') : t('expand'), 'more', 'expand', {
        id: message.id,
      }),
    );
  }

  const actions = document.createElement('div');
  actions.className = 'actions';
  for (const action of info.actions) {
    // Read-only archive: changing or deleting starts by pulling the monolog
    // out of the archive.
    if (app.archived && (action === 'edit' || action === 'delete' || action === 'retry')) {
      continue;
    }
    if (action === 'copy') {
      actions.append(actionButton(t('copy'), 'action', 'copy', { id: message.id }));
    } else if (action === 'edit') {
      actions.append(actionButton(t('edit'), 'action', 'edit', { id: message.id }));
    } else if (action === 'retry') {
      actions.append(actionButton(t('retry'), 'action', 'retry', { id: message.id }));
    } else if (action === 'delete') {
      actions.append(
        actionButton(t('delete'), 'action danger', 'delete', { id: message.id }),
      );
    }
  }
  if (actions.childElementCount > 0) card.append(actions);
  return card;
}

function unsentCard(take) {
  const card = document.createElement('article');
  card.className = 'message unsent';
  const time = document.createElement('div');
  time.className = 'time';
  time.append(document.createTextNode(shortTime(take.recorded)));
  time.append(chip(t('not_sent'), 'unsent'));
  card.append(time);
  const text = document.createElement('div');
  text.className = 'text empty';
  text.textContent = t('waiting_connection');
  card.append(text);
  const actions = document.createElement('div');
  actions.className = 'actions';
  actions.append(
    actionButton(t('delete'), 'action danger', 'discard', { local: take.localId }),
  );
  card.append(actions);
  return card;
}

async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast(t('copied'));
  } catch (error) {
    toast(t('copy_failed'));
  }
}

function toast(text) {
  const app = document.getElementById('app');
  const notes = document.getElementById('notes');
  const node = document.createElement('div');
  node.className = 'banner';
  node.textContent = text;
  // Notes belong to the layout, never on top of it: a fixed toast used to cover
  // the install button and the record bar on a phone. The pairing screen has no
  // shell, so there a note floats under the top edge instead.
  const host = notes && app && !app.hidden ? notes : document.body;
  if (host === document.body) node.classList.add('top-note');
  // The same failure repeats every tick; one note says it better than ten.
  for (const old of host.querySelectorAll('.banner')) {
    if (old.textContent === text) old.remove();
  }
  host.append(node);
  setTimeout(() => node.remove(), 2200);
}

/// The computer stopped answering, but the page still works. One note is
/// enough: the loop retries with backoff, and ten identical banners hide the
/// content they are about.
let offlineNotedAt = 0;
function offlineNote() {
  const now = Date.now();
  if (now - offlineNotedAt < 5000) return;
  offlineNotedAt = now;
  toast(t('offline_recording'));
}

/// Push a screen onto the browser's history. A phone's back gesture walks it,
/// so back leaves the monolog instead of hiding the whole app.
function pushView(state) {
  try {
    history.pushState(state, '');
  } catch (error) {
    /* a browser that refuses history still works, back just leaves the app */
  }
}

function leaveMonolog() {
  app.monolog = null;
  renderMonologs().catch(() => {});
}

function leaveArchive() {
  app.archiveTop = el('view').scrollTop;
  app.archived = false;
  renderMonologs().catch(() => {});
}

/// The top sheet goes away without touching the history: the entry is removed
/// by the caller, which knows whether it pushed one.
function dropSheet(token) {
  const index = app.sheets.findIndex((entry) => entry.token === token);
  if (index < 0) return false;
  const [entry] = app.sheets.splice(index, 1);
  entry.backdrop.remove();
  return true;
}

/// What back closes, shared by the header arrow and the phone's gesture.
function leaveOneScreen() {
  const target = backTarget({
    sheets: app.sheets.length,
    monolog: app.monolog,
    archived: app.archived,
  });
  if (target === 'sheet') {
    dropSheet(app.sheets[app.sheets.length - 1].token);
    return true;
  }
  if (target === 'monolog') {
    leaveMonolog();
    return true;
  }
  if (target === 'archive') {
    leaveArchive();
    return true;
  }
  return false;
}

function sheet(nodes) {
  const backdrop = document.createElement('div');
  backdrop.className = 'sheet-backdrop';
  const panel = document.createElement('div');
  panel.className = 'sheet';
  for (const node of nodes) panel.append(node);
  backdrop.append(panel);
  backdrop.addEventListener('click', (event) => {
    if (event.target === backdrop) dismissSheet(backdrop);
  });
  const token = ++app.sheetId;
  app.sheets.push({ token, backdrop });
  // A sheet owns one history entry: back closes the sheet and leaves the screen
  // under it alone, and the monolog keeps its own entry for the next back.
  pushView({ view: 'sheet', token });
  document.body.append(backdrop);
  return backdrop;
}

/// Close a sheet the way its own buttons do. The entry it pushed is spent, so
/// the browser and the page stay in step.
function dismissSheet(backdrop) {
  const entry = app.sheets.find((item) => item.backdrop === backdrop);
  if (!entry) {
    backdrop.remove();
    return;
  }
  if (history.state && history.state.view === 'sheet' && history.state.token === entry.token) {
    history.back();
    return;
  }
  dropSheet(entry.token);
}

function editSheet(message) {
  const title = document.createElement('h2');
  title.textContent = t('edit_title');
  const hint = document.createElement('div');
  hint.className = 'hint';
  hint.textContent = t('edit_hint');
  const area = document.createElement('textarea');
  area.value = message.text;
  area.autocomplete = 'off';
  area.spellcheck = false;
  const buttons = document.createElement('div');
  buttons.className = 'buttons';
  const dialog = sheet([title, hint, area, buttons]);
  const close = () => dismissSheet(dialog);
  buttons.append(button(t('cancel'), '', close));
  buttons.append(
    button(t('save'), 'primary', async () => {
      try {
        await call('PATCH', '/messages/' + message.id, { text: area.value });
        close();
        await app.sync.tick();
      } catch (error) {
        toast(t('offline'));
      }
    })
  );
  area.focus();
}

function deleteSheet(message) {
  const title = document.createElement('h2');
  title.textContent = t('delete_title');
  const hint = document.createElement('div');
  hint.className = 'hint';
  hint.textContent = t('delete_hint');
  const buttons = document.createElement('div');
  buttons.className = 'buttons';
  const dialog = sheet([title, hint, buttons]);
  const close = () => dismissSheet(dialog);
  buttons.append(button(t('cancel'), '', close));
  buttons.append(
    button(t('delete'), 'danger', async () => {
      try {
        await call('DELETE', '/messages/' + message.id);
        await app.cache.deleteMessage(
          Number(message.monolog ?? app.monolog?.id ?? 0),
          message,
        );
        removeFeedMessage(message.id);
        close();
        await app.sync.tick();
      } catch (error) {
        toast(t('offline'));
      }
    })
  );
}

function monologMenuSheet(monolog) {
  const title = document.createElement('h2');
  title.textContent = monolog.name;
  if (app.archived) {
    // Read-only archive: one action, no renaming and no recording.
    const hint = document.createElement('div');
    hint.className = 'hint';
    hint.textContent = t('archive_read_only');
    const buttons = document.createElement('div');
    buttons.className = 'buttons';
    const dialog = sheet([title, hint, buttons]);
    buttons.append(button(t('cancel'), '', () => dismissSheet(dialog)));
    buttons.append(
      button(t('unarchive'), 'primary', async () => {
        try {
          await call('POST', '/monologues/' + monolog.id + '/archive', { archived: false });
          await app.cache.setArchived(monolog.id, false);
          dismissSheet(dialog);
          app.monolog = null;
          app.sync.show(0);
          await renderMonologs();
          if (history.state && history.state.view === 'monolog') history.back();
        } catch (error) {
          toast(t('offline'));
        }
      }),
    );
    return;
  }
  // Same textarea trick as the create sheet: no autofill bar over the keys.
  const input = document.createElement('textarea');
  input.rows = 1;
  input.className = 'name';
  input.value = monolog.name;
  input.autocomplete = 'off';
  input.spellcheck = false;
  input.setAttribute('autocapitalize', 'sentences');
  input.setAttribute('enterkeyhint', 'done');
  const buttons = document.createElement('div');
  buttons.className = 'buttons';
  const second = document.createElement('div');
  second.className = 'buttons';
  const dialog = sheet([title, input, buttons, second]);
  const close = () => dismissSheet(dialog);
  buttons.append(button(t('cancel'), '', close));
  buttons.append(
    button(t('save'), 'primary', async () => {
      const name = input.value.trim();
      if (!name || name === monolog.name) {
        close();
        return;
      }
      // The name is local first. A monolog the computer has never seen only
      // changes here; one it knows keeps `nameDirty` and the sync loop sends it
      // when the connection is back. Renaming never needs the computer.
      await app.cache.rename(monolog.id, name);
      app.monolog = { id: monolog.id, name, local: monolog.local };
      close();
      header(name, { back: true, composer: true, menu: true });
      renderMonologs().catch(() => {});
      if (!monolog.local) {
        app.sync.tick().catch(() => {});
      }
    })
  );
  if (!monolog.local) {
    // The same button does both jobs, and its label says which one it is:
    // in the archive a monolog can only be pulled out of it.
    const inArchive = app.archived;
    second.append(
      button(inArchive ? t('unarchive') : t('archive'), '', async () => {
        try {
          await call('POST', '/monologues/' + monolog.id + '/archive', {
            archived: !inArchive,
          });
          // The record leaves this list immediately; the next fetch confirms.
          await app.cache.setArchived(monolog.id, !inArchive);
          close();
          // Leaving the chat keeps the browser history in step.
          app.monolog = null;
          app.sync.show(0);
          await renderMonologs();
          if (history.state && history.state.view === 'monolog') history.back();
        } catch (error) {
          toast(t('offline'));
        }
      })
    );
  }
}

function createMonologSheet() {
  const title = document.createElement('h2');
  title.textContent = t('new_monolog');
  // A textarea, not an input: Chrome offers the address book, bank cards and
  // passwords over a text input no matter what `autocomplete` says. The edit
  // sheet (a textarea) never showed those buttons — same field here.
  const input = document.createElement('textarea');
  input.rows = 1;
  input.className = 'name';
  input.placeholder = t('monolog_name');
  input.autocomplete = 'off';
  input.spellcheck = false;
  input.setAttribute('autocapitalize', 'sentences');
  input.setAttribute('enterkeyhint', 'done');
  const buttons = document.createElement('div');
  buttons.className = 'buttons';
  const dialog = sheet([title, input, buttons]);
  const close = () => dismissSheet(dialog);
  buttons.append(button(t('cancel'), '', close));
  buttons.append(
    button(t('create'), 'primary', async () => {
      const name = input.value.trim();
      if (!name) return;
      try {
        await call('POST', '/monologues', { name });
        close();
      } catch (error) {
        if (!(error instanceof Offline)) {
          toast(reason(error));
          return;
        }
        // The computer is away: the monolog stays on the phone with a negative
        // id and the sync loop creates it there when the connection is back.
        await app.cache.allocate(name);
        close();
        offlineNote();
      }
      await renderMonologs(0);
    })
  );
  input.focus();
}

async function retryMessage(message) {
  try {
    await call('POST', '/messages/' + message.id + '/retry');
    await app.sync.tick();
  } catch (error) {
    toast(t('offline'));
  }
}

/// Throw away a take that never left the phone — or one that reached the
/// computer but never finished. Both halves are removed, so nothing is left
/// hanging in the outbox.
async function discardTake(take) {
  if (app.recording && app.recording.take.localId === take.localId) {
    await stopRecording();
  }
  if (take.remote) {
    try {
      await call('DELETE', '/messages/' + take.remote);
    } catch (error) {
      /* the server may already be gone; the local copy still has to go */
    }
    try {
      await app.cache.deleteMessage(
        Number(app.monolog ? app.monolog.id : take.monolog),
        { recorded: take.recorded, id: take.remote },
      );
    } catch (error) {
      /* the page may already have dropped this record */
    }
    removeFeedMessage(take.remote);
  }
  await app.outbox.remove(take.localId);
  app.loop.kick();
  const feed = app.feed;
  if (feed) {
    rememberAnchor(feed);
    feed.pending = (await app.outbox.list()).filter(
      (item) => Number(item.monolog) === Number(feed.monolog.id) && !item.remote,
    );
    rebuildFeedRows(feed);
    paintVirtual(feed);
  }
}

// ---------------------------------------------------------------- composer

function updateComposer() {
  const recording = app.recording;
  const button = el('record');
  // The bar follows the recording to every screen: leaving the monolog must
  // not hide the fact that the microphone is still on.
  const bar = el('rec-bar');
  if (bar) {
    const take = recording ? recording.take : null;
    const here = Boolean(take && app.monolog && app.monolog.id === take.monolog);
    const show = Boolean(take) && !here;
    bar.hidden = !show;
    if (show) {
      el('rec-bar-text').textContent = t('recording');
      el('rec-bar-open').textContent = t('open');
    }
  }
  // Violet means "the computer is getting this". While it cannot be reached
  // the recording still continues, but the colour tells the truth instead of
  // pretending: violet would be a lie, so it turns amber.
  const working = !!recording && app.reachable;
  button.classList.toggle('recording', working);
  button.classList.toggle('offline', !!recording && !app.reachable);
  button.disabled = !app.monolog;
  el('rec-title').textContent = recording
    ? app.reachable
      ? t('recording')
      : t('recording_offline')
    : t('start_record');
  el('rec-sub').textContent = recording
    ? app.reachable
      ? t('stop_hint')
      : t('record_offline')
    : t('dictate_hint');
}

/// The recorder: microphone → 16 kHz mono → PCM16 chunks → IndexedDB → server.
function trace(what) {
  window.__trace = window.__trace || [];
  window.__trace.push(what);
}

async function startRecording() {
  trace('start');
  if (app.recording || !app.monolog || app.archived) return;
  const take = app.outbox.newTake(app.monolog.id);
  take.seq = 0;
  take.duration = 0;
  await app.outbox.put(take);
  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({
      audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
    });
  } catch (error) {
    await app.outbox.remove(take.localId);
    toast(t('mic_denied'));
    return;
  }
  const context = new AudioContext();
  await context.resume();
  if (navigator.wakeLock) {
    try {
      app.wakeLock = await navigator.wakeLock.request('screen');
    } catch (error) {
      /* the screen may still sleep; the recording survives in IndexedDB */
    }
  }
  const source = context.createMediaStreamSource(stream);
  const processor = context.createScriptProcessor(4096, 1, 1);
  const ratio = context.sampleRate / RATE;
  let carry = [];
  processor.onaudioprocess = (event) => {
    if (!window.__micSeen) {
      window.__micSeen = true;
      trace('mic');
    }
    const input = event.inputBuffer.getChannelData(0);
    for (let index = 0; index < input.length; index += ratio) {
      let sum = 0;
      let count = 0;
      for (let inner = 0; inner < ratio && index + inner < input.length; inner += 1) {
        sum += input[index + inner];
        count += 1;
      }
      carry.push(count ? sum / count : 0);
    }
    while (carry.length >= CHUNK_SAMPLES) {
      const slice = carry.slice(0, CHUNK_SAMPLES);
      carry = carry.slice(CHUNK_SAMPLES);
      enqueue(take, slice);
    }
  };
  source.connect(processor);
  processor.connect(context.destination);
  // A new recording always shows its own screen expanded and forgets which
  // older long message was open.
  app.expandedMessage = null;
  app.recordingMonolog = { ...app.monolog };
  app.recording = {
    take,
    context,
    stream,
    processor,
    source,
    started: Date.now(),
    takeTail: () => {
      if (carry.length > 0) {
        enqueue(take, carry);
        carry = [];
      }
    },
  };
  updateComposer();
  tick();
  trace('recording');
  app.loop.kick();
}

function enqueue(take, samples) {
  trace('chunk');
  const buffer = new ArrayBuffer(samples.length * 2);
  const view = new Int16Array(buffer);
  for (let index = 0; index < samples.length; index += 1) {
    const value = Math.max(-1, Math.min(1, samples[index]));
    view[index] = value < 0 ? value * 0x8000 : value * 0x7fff;
  }
  const seq = take.seq;
  take.seq += 1;
  take.duration = (take.duration || 0) + samples.length / RATE;
  app.outbox
    .putChunk(take.localId, seq, buffer)
    .then(() => app.outbox.put(take))
    .then(() => app.loop.kick())
    .catch((error) => trace('put-error:' + error));
}

async function stopRecording() {
  const active = app.recording;
  if (!active) return;
  // The button stops looking like a recording before any slow cleanup: a
  // failure below must never leave it violet forever.
  app.recording = null;
  updateComposer();
  try {
    active.processor.disconnect();
    active.source.disconnect();
    active.stream.getTracks().forEach((track) => track.stop());
    await active.context.close();
  } catch (error) {
    /* closing a closed context is not an error worth reporting */
  }
  if (app.wakeLock) {
    try {
      await app.wakeLock.release();
    } catch (error) {
      /* released automatically when the page is hidden */
    }
  }
  // Whatever is left in the buffer is a real part of the take.
  try {
    if (active.takeTail) active.takeTail();
  } catch (error) {
    trace('tail-error:' + error);
  }
  active.take.closed = true;
  try {
    await app.outbox.put(active.take);
  } catch (error) {
    trace('stop-error:' + error);
  }
  app.recordingMonolog = null;
  updateComposer();
  app.loop.kick();
}

function tick() {
  if (!app.recording) {
    el('timer').textContent = '';
    return;
  }
  const seconds = Math.floor((Date.now() - app.recording.started) / 1000);
  const clock = Math.floor(seconds / 60) + ':' + String(seconds % 60).padStart(2, '0');
  el('timer').textContent = clock;
  const barText = el('rec-bar-text');
  if (barText && !el('rec-bar').hidden) {
    barText.textContent = t('recording') + ' · ' + clock;
  }
  setTimeout(tick, 500);
}

// ---------------------------------------------------------------- boot

/// Is the page already running as the installed app? On Android it is the
/// display mode, on iPhone Safari its own flag.
function runningAsApp() {
  try {
    return (
      window.matchMedia('(display-mode: standalone)').matches ||
      window.matchMedia('(display-mode: minimal-ui)').matches ||
      window.matchMedia('(display-mode: fullscreen)').matches ||
      window.matchMedia('(display-mode: window-controls-overlay)').matches ||
      window.navigator.standalone === true
    );
  } catch (error) {
    return false;
  }
}

/// Show the footer's install entry only when there is something to install.
function refreshInstallEntry() {
  const entry = installEntry({
    standalone: runningAsApp(),
    installed: app.installed,
    hasPrompt: Boolean(app.installPrompt),
  });
  applyInstallEntry(entry, {
    link: el('to-install'),
    button: el('install'),
    about: document.querySelector('footer.about'),
  });
}

function setupInstall() {
  const link = el('to-install');
  if (link) link.textContent = t('install_app');
  const button = el('install');
  button.textContent = t('install');
  refreshInstallEntry();
  try {
    const media = window.matchMedia('(display-mode: standalone)');
    media.addEventListener('change', refreshInstallEntry);
  } catch (error) {
    /* older browsers: the first render is enough */
  }
  button.addEventListener('click', async () => {
    if (app.installPrompt) {
      app.installPrompt.prompt();
      app.installPrompt = null;
      refreshInstallEntry();
    }
  });
}

/// Drop every cached copy and reload: the next load must come from the
/// computer, not from an old service worker.
async function forceRefresh() {
  try {
    const registrations = await navigator.serviceWorker.getRegistrations();
    await Promise.all(registrations.map((registration) => registration.unregister()));
  } catch (error) {
    /* without service workers there is nothing to unregister */
  }
  try {
    for (const key of await caches.keys()) await caches.delete(key);
  } catch (error) {
    /* no Cache Storage: the reload is still worth it */
  }
  location.reload();
}

/// Compare the page with the version the computer serves. A page that was
/// installed once used to keep its old copy forever; this check is what makes
/// "открой приложение при связи" actually update it.
async function checkForUpdate() {
  const button = el('update-app');
  if (!button) return;
  let served = null;
  try {
    const answer = await fetch('version.json', { cache: 'no-store' });
    if (!answer.ok) return;
    served = (await answer.json()).site;
  } catch (error) {
    // Offline, or the computer is away: try again on the next visibility.
    return;
  }
  if (!served || served === SITE_VERSION) {
    button.hidden = true;
    return;
  }
  button.textContent = t('update_app');
  button.hidden = false;
  sessionStorage.setItem('vb-seen-version', served);
  const alreadyTried = sessionStorage.getItem('vb-tried-version');
  if (alreadyTried !== served) {
    sessionStorage.setItem('vb-tried-version', served);
    await forceRefresh();
    return;
  }
  // The reload did not help (the computer still serves another build): let the
  // person decide instead of looping.
  button.addEventListener('click', forceRefresh, { once: true });
}

async function registerServiceWorker() {
  if (!('serviceWorker' in navigator)) return;
  try {
    // `updateViaCache: 'none'` makes the browser ask the computer for sw.js
    // instead of trusting a cached copy: an installed app can sit offline for
    // days, and the old shell must not survive the next online start.
    const registration = await navigator.serviceWorker.register('sw.js', {
      updateViaCache: 'none',
    });
    const hadController = Boolean(navigator.serviceWorker.controller);
    let reloaded = false;
    navigator.serviceWorker.addEventListener('controllerchange', () => {
      // The first install also claims the page; only a replacement of an older
      // shell is worth a reload.
      if (!hadController || reloaded) return;
      reloaded = true;
      location.reload();
    });
    registration.update().catch(() => {});
    document.addEventListener('visibilitychange', () => {
      if (!document.hidden) registration.update().catch(() => {});
    });
  } catch (error) {
    /* installation still works, the app is just not cached */
  }
}

/// The holiday microphone for today, drawn on the record button and as the tab
/// icon. Both come from the computer, from the same SVG file the tray draws, so
/// the phone and the tray can never show different artwork. The button's colour
/// is the icon's colour, which is why the microphone follows recording by itself.
/// Nothing is drawn on an ordinary day, and a failure here must not delay the page.
async function applyHoliday() {
  const answer = await call('GET', '/me');
  const id = answer.holiday;
  const record = el('record');
  if (!id || !record) return;
  const response = await fetch(API + '/holiday.svg', {
    headers: { Authorization: 'Bearer ' + app.token },
  });
  if (!response.ok) return;
  const svg = await response.text();
  // The markup is our own file, served by this computer, not user content.
  record.innerHTML = svg;
  const glyph = record.firstElementChild;
  if (glyph) {
    glyph.classList.add('glyph');
    glyph.setAttribute('aria-hidden', 'true');
  }
  const favicon = el('favicon');
  // A bare microphone has nothing to inherit from in a tab, so it is given the
  // brand colour there.
  if (favicon) {
    favicon.href = 'data:image/svg+xml,' + encodeURIComponent(svg.replace('<svg ', '<svg color="#c7f36b" '));
  }
}

async function boot() {
  await loadLanguage();
  if (!app.token) {
    showPairError();
    return;
  }
  applyHoliday().catch(() => {});
  try {
    const storage = await openStorage();
    app.cache = createCache({
      monologs: storage.monologs,
      messages: storage.messages,
      now: localIso,
    });
    app.outbox = createOutbox({ store: storage.store, chunks: storage.chunks, now: localIso });
    // A page that was closed or killed mid-take leaves an open take behind. The
    // recording is over (there is no microphone now): close it so the phone
    // sends `finish` and the whole audio is transcribed instead of hanging.
    for (const take of await app.outbox.list()) {
      if (!take.closed) await app.outbox.put({ ...take, closed: true });
    }
    app.sync = createSync({
      api: call,
      outbox: app.outbox,
      render: draw,
      cache: app.cache,
      // The loop fetches the first list page; the page only draws what is
      // stored and pages in the rest as the reader scrolls.
      renderList: async (page, archivedView) => {
        // A list fetched before the archive was opened must not paint itself
        // over the archive: only the answer for the view on screen is drawn.
        if (app.monolog || app.archived !== archivedView || app.screen !== 'list') {
          return;
        }
        const state = app.listState;
        if (!state || state.archived !== archivedView) return;
        await refreshListState(state);
        state.pending = await app.outbox.list();
        state.rows = buildListRows(state);
        paintVirtual(state);
      },
      // A local monolog created on the computer while it is open here gets its
      // real id: the header, the history entry and the messages follow it.
      mapped: (localId, serverId) => {
        if (!app.monolog || Number(app.monolog.id) !== Number(localId)) return;
        app.monolog = { id: serverId, name: app.monolog.name, local: false };
        if (app.feed) {
          app.feed.monolog = { ...app.feed.monolog, id: serverId, local: false };
          app.feed.total = 0;
          app.feed.hasOlder = false;
          app.feed.loaded = [];
          app.feed.byId.clear();
        }
        try {
          history.replaceState(viewState({ monolog: app.monolog }), '');
        } catch (error) {
          /* a browser without history still records */
        }
        header(app.monolog.name, { back: true, composer: true, menu: true });
      },
      log: (message) => {
        console.warn(message);
        trace('sync:' + message);
      },
    });
    app.loop = createLoop({
      tick: async () => {
        trace('tick');
        const result = await app.sync.tick();
        // A refusal from the computer (a take it already finished) is not
        // "the computer is offline"; only a real lost connection deserves that
        // warning every few seconds.
        if (result && result.failed && result.offline === true) offlineNote();
        trace('tick-done:' + JSON.stringify(result) + ':sched=' + app.loop.scheduled);
        return result;
      },
      onError: (error) => {
        if (!(error instanceof Offline)) console.warn(error);
      },
    });
    // The end-to-end script reads the outbox through this: a phone has no
    // console, so when a take loses its audio this is the only way to see
    // which takes and chunks the page actually kept.
    window.app = app;
  } catch (error) {
    fail(reason(error));
    return;
  }

  // The cache is the first thing on screen: the phone opens with what it last
  // knew even when the computer is gone.
  await renderMonologs();
  let offline = false;
  try {
    await call('GET', '/me');
  } catch (error) {
    const outcome =
      error instanceof Offline
        ? bootOutcome({ hasToken: true, offline: true, hasCache: await app.cache.hasData() })
        : 'error';
    if (outcome !== 'app') {
      fail(reason(error));
      return;
    }
    offline = true;
    // Say it once, right away: the list may be empty on a brand-new phone and
    // the user must know why the computer's monologs are missing.
    offlineNote();
  }

  el('record').addEventListener('click', () => {
    if (app.recording) stopRecording();
    else startRecording();
  });
  el('rec-bar').addEventListener('click', () => {
    if (app.recordingMonolog) openMonolog(app.recordingMonolog);
  });
  el('back').addEventListener('click', () => {
    if (history.state && history.state.view && history.state.view !== 'list') {
      history.back();
    } else {
      leaveOneScreen();
    }
  });
  el('archive').addEventListener('click', () => {
    if (!app.archived) app.listTop = el('view').scrollTop;
    app.archived = true;
    if (!history.state || history.state.view !== 'archive') {
      pushView(viewState({ archived: true }));
    }
    renderMonologs().catch(() => {});
  });
  el('menu').addEventListener('click', () => {
    if (app.monolog) monologMenuSheet(app.monolog);
  });
  history.replaceState(history.state || { view: 'list' }, '');
  window.addEventListener('popstate', (event) => {
    // One rule: the browser says which screen we are back on, and the page
    // follows it. A sheet is closed first and the screen underneath stays.
    if (app.sheets.length > 0) {
      dropSheet(app.sheets[app.sheets.length - 1].token);
      return;
    }
    const state = event.state || { view: 'list' };
    if (state.view === 'monolog') return;
    if (state.view === 'archive') {
      // Back from a monolog that was opened inside the archive lands on the
      // archive, not on the active list.
      if (app.monolog) leaveMonolog();
      if (!app.archived) {
        app.archived = true;
        renderMonologs().catch(() => {});
      }
      return;
    }
    if (app.monolog) leaveMonolog();
    else if (app.archived) leaveArchive();
  });
  window.addEventListener('online', () => {
    // The computer may be reachable again: let the microphone show it and
    // give the queue a fresh attempt, not the old backoff.
    setReachable(true);
    app.loop.kick();
    checkForUpdate();
  });
  // A phone whose screen went dark comes back here: send what waited at once.
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden) {
      app.loop.kick();
      checkForUpdate();
    }
  });
  window.addEventListener('offline', () => toast(t('offline')));
  setupInstall();
  app.loop.start();
  if (offline) offlineNote();
  // Registration is an optimisation; a stalled update must not keep the list
  // off the screen (that is how a reload could leave an empty page).
  registerServiceWorker();
  checkForUpdate();
}

// A boot that cannot finish (a blocked IndexedDB, a service worker waking up
// on a dead connection) must never leave a black square. After a few seconds
// the empty shell is shown; the loop fills it in when it can.
setTimeout(() => {
  try {
    if (!el('app').hidden || !el('pair-error').hidden) return;
    trace('boot-watchdog');
    header(t('monologs'), {});
    app.screen = 'list';
    app.monolog = null;
    const state = createListState(false);
    state.rows = buildListRows(state);
    app.listState = state;
    mountScreen();
    paintVirtual(state);
    offlineNote();
  } catch (error) {
    trace('watchdog-error:' + error);
  }
}, 6000);

el('reload').addEventListener('click', () => location.reload());

// A phone has no console: an exception must end up on the screen.
window.addEventListener('error', (event) => {
  if (el('app').hidden && el('pair-error').hidden) fail(reason(event.error || event.message));
});
window.addEventListener('unhandledrejection', (event) => {
  if (el('app').hidden && el('pair-error').hidden) fail(reason(event.reason));
});
// If nothing managed to render at all, say so instead of showing white.
setTimeout(() => {
  if (el('app').hidden && el('pair-error').hidden) fail(t('cannot_reach'));
}, 6000);

boot().catch((error) => fail(reason(error)));
