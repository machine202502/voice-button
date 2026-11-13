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
import { buildFeed, dayKey } from './chat.mjs';
import { atBottom, nextScroll } from './scroll.mjs';
import { SITE_VERSION } from './version.mjs';
import { Offline } from './offline.mjs';
import { bootOutcome, createCache, mergeMonologs } from './cache.mjs';

const API = '/site/api';
const RATE = 16000;
const CHUNK_SECONDS = 0.5;
const CHUNK_SAMPLES = Math.round(RATE * CHUNK_SECONDS);

// The token may arrive in the fragment when the page is moved between the
// public and the local address: fragments never reach the server.
if (location.hash.startsWith('#t=')) {
  const moved = decodeURIComponent(location.hash.slice(3));
  if (moved) localStorage.setItem('vb_token', moved);
  history.replaceState(null, '', location.pathname + location.search);
}

const app = {
  token: localStorage.getItem('vb_token') || '',
  lang: 'ru',
  strings: {},
  outbox: null,
  sync: null,
  loop: null,
  cache: null,
  monologs: [],
  monolog: null,
  messages: [],
  archived: false,
  recording: null,
  installPrompt: null,
  installed: false,
  sheets: [],
  sheetId: 0,
  chatFirst: false,
  expandedMessage: null,
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
  const preferred = (navigator.language || 'ru').slice(0, 2).toLowerCase();
  app.lang = ['ru', 'en', 'zh'].includes(preferred) ? preferred : 'en';
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
}

function t(key, fallback) {
  return app.strings[key] || fallback || key;
}

/// Message counts need real plural forms in Russian, and two of them in
/// English. Chinese has one.
function countLabel(count) {
  const rest10 = count % 10;
  const rest100 = count % 100;
  let form = 'many';
  if (app.lang === 'ru') {
    if (rest10 === 1 && rest100 !== 11) form = 'one';
    else if (rest10 >= 2 && rest10 <= 4 && (rest100 < 12 || rest100 > 14)) form = 'few';
  } else if (app.lang === 'en') {
    form = count === 1 ? 'one' : 'many';
  }
  return count + ' ' + t('messages_' + form, 'messages');
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
  fail(t('pair_required', 'Телефон не подключён. Попросите новый QR-код на компьютере.'));
}

function reason(error) {
  if (error instanceof Offline) {
    return t('offline_long', 'Компьютер не отвечает. Проверьте, что он включён и в той же сети.');
  }
  const message = String((error && error.message) || error || '');
  if (message.includes('local network')) {
    return t('lan_blocked', 'Компьютер отклонил подключение: он принимает только из локальной сети.');
  }
  if (message.includes('pairing required')) {
    return t('pair_required', 'Телефон не подключён. Попросите новый QR-код на компьютере.');
  }
  return t('cannot_reach', 'Компьютер недоступен.') + (message ? ' · ' + message : '');
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
  if (key === localKey(today)) return t('today', 'Сегодня');
  if (key === localKey(new Date(today.getTime() - 86400000))) return t('yesterday', 'Вчера');
  const date = new Date(key + 'T00:00:00');
  if (Number.isNaN(date.getTime())) return key;
  return date.toLocaleDateString(app.lang, { weekday: 'short', day: '2-digit', month: '2-digit' });
}

// ---------------------------------------------------------------- views

/// Put a rendered fragment on screen.
///
/// `mode` is `chat` for a monolog: it opens at the newest message and follows
/// new ones only while the reader is already at the bottom. A redraw must never
/// pull someone away from the message they are reading.
function show(view, mode = 'keep', forcedTop = null) {
  const page = el('view');
  const wasAtBottom = atBottom({
    offset: page.scrollTop,
    height: page.scrollHeight,
    viewport: page.clientHeight,
  });
  const previous = page.scrollTop;
  el('app').hidden = false;
  el('pair-error').hidden = true;
  page.replaceChildren(view);
  page.scrollTop =
    forcedTop !== null
      ? forcedTop
      : nextScroll({
          mode,
          first: app.chatFirst === true,
          atBottom: wasAtBottom,
          height: page.scrollHeight,
          viewport: page.clientHeight,
          previous,
        });
  if (mode === 'chat') app.chatFirst = false;
}

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

/// Draw the list from what is stored. The refresh from the computer is the sync
/// loop's work, so this never fails on a dead connection.
function drawMonologs(monologs, pending, top = null) {
  app.monologs = monologs;
  const view = document.createDocumentFragment();
  if (monologs.length === 0) {
    view.append(empty(app.archived ? t('archive_empty', 'В архиве пусто.') : t('no_monologs', 'Пока нет монологов. Создайте первый — и диктуйте в него.')));
  }
  for (const monolog of monologs) {
    const card = document.createElement('button');
    card.className = 'monolog-card';
    card.innerHTML =
      '<div class="top"><strong></strong><span class="when"></span></div>' +
      '<div class="last"></div><div class="count"></div>';
    card.querySelector('strong').textContent = monolog.name;
    if (monolog.local) {
      card.querySelector('.top').append(chip(t('waiting_link', 'ждёт связи'), 'unsent'));
    }
    // Takes that never reached the computer are not in its count yet, so the
    // card has to add them: an offline monolog used to show "0 messages" right
    // after the user dictated into it.
    const waiting = (pending || []).filter(
      (take) => take.monolog === monolog.id && !take.remote
    ).length;
    card.querySelector('.count').textContent = countLabel(
      monolog.local ? waiting : (monolog.count || 0) + waiting
    );
    // A monolog with work in it is visible from the list: recording here, or
    // the computer still transcribing.
    if (
      monolog.busy ||
      (app.recording && app.recording.take.monolog === monolog.id)
    ) {
      card.classList.add('active');
    }
    card.addEventListener('click', () => openMonolog(monolog));
    view.append(card);
  }
  if (!app.archived) {
    const create = document.createElement('button');
    create.className = 'new-monolog';
    create.textContent = t('new_monolog', '+ Новый монолог');
    create.addEventListener('click', createMonologSheet);
    view.append(create);
  }
  show(view, 'keep', top);
}

async function renderMonologs() {
  app.monolog = null;
  app.sync.show(0);
  app.sync.archive(app.archived);
  header(app.archived ? t('archive', 'Архив') : t('monologs', 'Монологи'), {
    hideArchive: app.archived,
  });
  updateComposer();
  const local = app.archived ? [] : await app.cache.localMonologs();
  const server = await app.cache.serverMonologs(app.archived);
  const pending = await app.outbox.list();
  drawMonologs(mergeMonologs(server, local), pending, app.archived ? app.archiveTop : app.listTop);
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
  app.chatFirst = true;
  app.sync.show(monolog.id);
  // An archived monolog can be read and pulled out of the archive, never
  // changed or recorded into.
  header(monolog.name, { back: true, composer: !app.archived, menu: true });
  updateComposer();
  // Cached messages and the waiting takes are on the screen before the
  // computer is asked, so opening a monolog offline is never a blank wait.
  draw(await app.cache.messages(monolog.id), await app.outbox.list());
  try {
    await app.sync.tick();
  } catch (error) {
    if (error instanceof Offline) offlineNote();
    else fail(reason(error));
  }
}

/// Draw the list the sync loop fetched. Rendering must never start a fetch or
/// a timer: the loop is the only thing that does that.
function draw(messages, pending) {
  if (!app.monolog) return;
  app.messages = messages;
  app.pending = pending || [];
  const view = document.createDocumentFragment();
  const rows = buildFeed(messages);
  const waiting = (pending || []).filter(
    (take) => !take.remote && take.monolog === app.monolog.id
  );
  if (rows.length === 0 && waiting.length === 0) {
    view.append(empty(t('no_messages', 'Здесь пока пусто. Нажмите микрофон и говорите.')));
  }
  for (const row of rows) {
    if (row.kind === 'day') {
      const separator = document.createElement('div');
      separator.className = 'day';
      separator.textContent = dayLabel(row.key);
      view.append(separator);
    } else {
      view.append(messageCard(row.message));
    }
  }
  for (const take of waiting) view.append(unsentCard(take));
  show(view, 'chat');
  updateComposer();
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
  if (waiting) time.append(chip(t('engine_busy', 'движок занят'), 'busy'));
  if (message.edited) time.append(chip(t('edited', 'изменено'), 'edited'));
  card.append(time);

  const long = Boolean(message.text) && message.text.length > 320;
  const expanded = app.expandedMessage === message.id && !app.recording;
  const text = document.createElement('div');
  text.className = 'text' + (message.text ? '' : ' empty');
  const hint = message.text
    ? ''
    : waiting
      ? t('busy_hint', 'Движок занят на компьютере — расшифровка продолжится, когда он освободится.')
      : info.hint
        ? t(info.hint, '')
        : '';
  text.textContent = message.text
    ? long && !expanded
      ? message.text.slice(0, 240).trimEnd() + '…'
      : message.text
    : hint;
  card.append(text);
  if (long && !app.recording) {
    card.append(
      button(expanded ? t('collapse', 'Свернуть') : t('expand', 'Развернуть'), 'more', () => {
        app.expandedMessage = expanded ? null : message.id;
        draw(app.messages, app.pending);
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
      actions.append(button(t('copy', 'Копировать'), 'action', () => copy(message.text)));
    } else if (action === 'edit') {
      actions.append(button(t('edit', 'Изменить'), 'action', () => editSheet(message)));
    } else if (action === 'retry') {
      actions.append(button(t('retry', 'Повторить'), 'action', () => retryMessage(message)));
    } else if (action === 'delete') {
      actions.append(button(t('delete', 'Удалить'), 'action danger', () => deleteSheet(message)));
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
  time.append(chip(t('not_sent', 'Не отправлено'), 'unsent'));
  card.append(time);
  const text = document.createElement('div');
  text.className = 'text empty';
  text.textContent = t('waiting_connection', 'Уйдёт, когда появится связь.');
  card.append(text);
  const actions = document.createElement('div');
  actions.className = 'actions';
  actions.append(button(t('delete', 'Удалить'), 'action danger', () => discardTake(take)));
  card.append(actions);
  return card;
}

async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast(t('copied', 'Скопировано'));
  } catch (error) {
    toast(t('copy_failed', 'Не удалось скопировать'));
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
  toast(t('offline_recording', 'Нет связи с компьютером — запись сохранится'));
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
  title.textContent = t('edit_title', 'Изменить сообщение');
  const hint = document.createElement('div');
  hint.className = 'hint';
  hint.textContent = t('edit_hint', 'Оригинал останется в истории приложения');
  const area = document.createElement('textarea');
  area.value = message.text;
  area.autocomplete = 'off';
  area.spellcheck = false;
  const buttons = document.createElement('div');
  buttons.className = 'buttons';
  const dialog = sheet([title, hint, area, buttons]);
  const close = () => dismissSheet(dialog);
  buttons.append(button(t('cancel', 'Отмена'), '', close));
  buttons.append(
    button(t('save', 'Сохранить'), 'primary', async () => {
      try {
        await call('PATCH', '/messages/' + message.id, { text: area.value });
        close();
        await app.sync.tick();
      } catch (error) {
        toast(t('offline', 'Нет связи с компьютером'));
      }
    })
  );
  area.focus();
}

function deleteSheet(message) {
  const title = document.createElement('h2');
  title.textContent = t('delete_title', 'Удалить сообщение?');
  const hint = document.createElement('div');
  hint.className = 'hint';
  hint.textContent = t('delete_hint', 'Оригинал останется в истории приложения.');
  const buttons = document.createElement('div');
  buttons.className = 'buttons';
  const dialog = sheet([title, hint, buttons]);
  const close = () => dismissSheet(dialog);
  buttons.append(button(t('cancel', 'Отмена'), '', close));
  buttons.append(
    button(t('delete', 'Удалить'), 'danger', async () => {
      try {
        await call('DELETE', '/messages/' + message.id);
        close();
        await app.sync.tick();
      } catch (error) {
        toast(t('offline', 'Нет связи с компьютером'));
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
    hint.textContent = t(
      'archive_read_only',
      'В архиве нельзя менять и записывать. Верните монолог, чтобы работать с ним.',
    );
    const buttons = document.createElement('div');
    buttons.className = 'buttons';
    const dialog = sheet([title, hint, buttons]);
    buttons.append(button(t('cancel', 'Отмена'), '', () => dismissSheet(dialog)));
    buttons.append(
      button(t('unarchive', 'Из архива'), 'primary', async () => {
        try {
          await call('POST', '/monologues/' + monolog.id + '/archive', { archived: false });
          await app.cache.setArchived(monolog.id, false);
          dismissSheet(dialog);
          app.monolog = null;
          app.sync.show(0);
          await renderMonologs();
          if (history.state && history.state.view === 'monolog') history.back();
        } catch (error) {
          toast(t('offline', 'Нет связи с компьютером'));
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
  buttons.append(button(t('cancel', 'Отмена'), '', close));
  buttons.append(
    button(t('save', 'Сохранить'), 'primary', async () => {
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
      button(inArchive ? t('unarchive', 'Из архива') : t('archive', 'В архив'), '', async () => {
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
          toast(t('offline', 'Нет связи с компьютером'));
        }
      })
    );
  }
}

function createMonologSheet() {
  const title = document.createElement('h2');
  title.textContent = t('new_monolog', '+ Новый монолог');
  // A textarea, not an input: Chrome offers the address book, bank cards and
  // passwords over a text input no matter what `autocomplete` says. The edit
  // sheet (a textarea) never showed those buttons — same field here.
  const input = document.createElement('textarea');
  input.rows = 1;
  input.className = 'name';
  input.placeholder = t('monolog_name', 'Название монолога');
  input.autocomplete = 'off';
  input.spellcheck = false;
  input.setAttribute('autocapitalize', 'sentences');
  input.setAttribute('enterkeyhint', 'done');
  const buttons = document.createElement('div');
  buttons.className = 'buttons';
  const dialog = sheet([title, input, buttons]);
  const close = () => dismissSheet(dialog);
  buttons.append(button(t('cancel', 'Отмена'), '', close));
  buttons.append(
    button(t('create', 'Создать'), 'primary', async () => {
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
      await renderMonologs();
    })
  );
  input.focus();
}

async function retryMessage(message) {
  try {
    await call('POST', '/messages/' + message.id + '/retry');
    await app.sync.tick();
  } catch (error) {
    toast(t('offline', 'Нет связи с компьютером'));
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
  }
  await app.outbox.remove(take.localId);
  app.loop.kick();
  draw(app.messages, await app.outbox.list());
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
      el('rec-bar-text').textContent = t('recording', 'Идёт запись');
      el('rec-bar-open').textContent = t('open', 'Открыть');
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
      ? t('recording', 'Идёт запись')
      : t('recording_offline', 'Запись без связи')
    : t('start_record', 'Нажмите, чтобы начать');
  el('rec-sub').textContent = recording
    ? app.reachable
      ? t('stop_hint', 'Нажмите, чтобы остановить')
      : t('record_offline', 'Нет связи — запись сохранится, расшифровка после подключения')
    : t('dictate_hint', 'Диктуйте в монолог');
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
    toast(t('mic_denied', 'Нет доступа к микрофону'));
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
    barText.textContent = t('recording', 'Идёт запись') + ' · ' + clock;
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
  if (link) link.textContent = t('install_app', 'Установить приложение');
  const button = el('install');
  button.textContent = t('install', 'Добавить на главный экран');
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
  button.textContent = t('update_app', 'Обновить');
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

async function boot() {
  await loadLanguage();
  if (!app.token) {
    showPairError();
    return;
  }
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
      // The loop fetches the list; the page only draws what it is given.
      renderList: async (server, archivedView) => {
        // A list fetched before the archive was opened must not paint itself
        // over the archive: only the answer for the view on screen is drawn.
        if (app.monolog || app.archived !== archivedView) return;
        const local = app.archived ? [] : await app.cache.localMonologs();
        const pending = await app.outbox.list();
        drawMonologs(mergeMonologs(server, local), pending);
      },
      // A local monolog created on the computer while it is open here gets its
      // real id: the header, the history entry and the messages follow it.
      mapped: (localId, serverId) => {
        if (!app.monolog || app.monolog.id !== localId) return;
        app.monolog = { id: serverId, name: app.monolog.name, local: false };
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
  window.addEventListener('offline', () => toast(t('offline', 'Нет связи с компьютером')));
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
    header(t('monologs', 'Монологи'), {});
    drawMonologs([], []);
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
  if (el('app').hidden && el('pair-error').hidden) fail(t('cannot_reach', 'Компьютер недоступен.'));
}, 6000);

boot().catch((error) => fail(reason(error)));
