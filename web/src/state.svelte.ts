// The phone page's state: one object the whole UI reads and the only place the
// screens and the data layer meet.
//
// The data layer under `data/` knows how to store, queue and send; the screens
// know how to draw. Everything in between — which screen is open, which paged
// window is loaded, what the microphone is doing, what a click means — lives
// here, so a screen never talks to the network and never decides what to queue.
//
// The rules this module exists to keep:
//
//   * a manual change is local first: the cache takes it, the queue takes it,
//     the UI redraws from the cache, and only then is the sync loop asked to
//     send. Nothing waits for the computer, so a click is never lost and never
//     blocks;
//   * the sync loop is the only repeating work, and its answers come back
//     through `render`/`renderList` into this state, never straight into a
//     component;
//   * the recording is recorder → IndexedDB chunk by chunk → sync loop, so a
//     take survives a lost connection, a reload and a closed tab.

import { tick } from 'svelte';
import {
  OfflineError,
  PairingError,
  createApi,
} from './data/api';
import { createCache } from './data/cache';
import type { Cache, MessagePage } from './data/cache';
import { createMutations } from './data/mutations';
import type { MutationQueue } from './data/mutations';
import { createOutbox } from './data/outbox';
import type { Outbox } from './data/outbox';
import { createSync } from './data/sync';
import type { Api, ListResult, MutationFailure, Sync, TickResult } from './data/sync';
import { openStorage } from './data/storage';
import type { Storage } from './data/storage';
import type { Message, Monolog, Take } from './data/types';
import { language, setLanguage, t } from './i18n';
import { messageId, textOf } from './lib/format';
import { applyTheme, readTheme } from './lib/theme';
import type { ThemeMode } from './lib/theme';
import { version as pageVersion } from './lib/version';
import { NoiseCleaner } from './lib/noise';
import { readToken } from './lib/token';

/** How many rows one screen keeps in memory. Older pages are dropped. */
const LIST_PAGE = 100;
const FEED_PAGE = 50;
const FEED_MAX = 300;
const LIST_MAX = 300;
/** Start loading the next page this close to the end of the scroll. */
const LOAD_MORE_PX = 700;
const RECORD_RATE = 16000;
const CHUNK_SAMPLES = Math.round(RECORD_RATE * 0.5);
/** How often the level for the rings is measured; 30 Hz is smooth to the eye. */
const METER_MS = 33;
const METER_SECONDS = METER_MS / 1000;

/** A monolog as the page refers to it while one is open. */
export interface MonologRef {
  id: number;
  name: string;
  local: boolean;
}

/** One list view as it is drawn: the loaded rows, the waiting takes, the total. */
export interface ListData {
  records: Monolog[];
  pending: Take[];
  total: number;
  loading: boolean;
}

/** The feed of the open monolog. */
export interface FeedData {
  loaded: Message[];
  pending: Take[];
  total: number;
  hasOlder: boolean;
  loadingOlder: boolean;
  /** Bumped when the feed should follow its newest row. */
  scrollTick: number;
  atBottom: boolean;
}

/** What the microphone is doing right now. */
export interface RecordingView {
  take: Take;
  /** The monolog it is being recorded into; follows a local→server mapping. */
  monologId: number;
  name: string;
  started: number;
}

export type SheetKind = 'create' | 'menu' | 'edit' | 'delete';

/** One open sheet. Data only: `SheetHost` decides the buttons for each kind. */
export interface SheetData {
  kind: SheetKind;
  title: string;
  hint: string;
  /** The one text field, when the sheet has one. */
  field: string | null;
  placeholder: string;
  /** The row the sheet acts on. */
  monolog: Monolog | null;
  message: Message | null;
  /** The sheet was opened from the archive, where the monolog is read-only. */
  archivedView: boolean;
}

export interface Note {
  id: number;
  text: string;
}

export type PageMode = 'boot' | 'pair' | 'app';
export type Screen = 'list' | 'feed';

interface HistoryView {
  view?: string;
  id?: number;
  token?: number;
}

/** The audio side of a recording; kept out of reactive state on purpose. */
interface ActiveAudio {
  take: Take;
  context: AudioContext;
  stream: MediaStream;
  processor: ScriptProcessorNode;
  source: MediaStreamAudioSourceNode;
  wakeLock: WakeLockLike | null;
  tail: () => void;
}

interface WakeLockLike {
  release: () => Promise<void>;
}

interface WakeLockApi {
  request: (type: 'screen') => Promise<WakeLockLike>;
}

interface InstallPrompt extends Event {
  prompt: () => Promise<void>;
}

/** The modules the page built at boot, once they exist. */
interface Own {
  storage: Storage;
  cache: Cache;
  outbox: Outbox;
  mutations: MutationQueue;
  sync: Sync;
  api: Api;
}

function emptyList(): ListData {
  return { records: [], pending: [], total: 0, loading: false };
}

function viewNode(): HTMLElement | null {
  const node = document.getElementById('view');
  return node instanceof HTMLElement ? node : null;
}

function historyView(): HistoryView | null {
  const state: unknown = history.state;
  if (state === null || typeof state !== 'object') return null;
  return state as HistoryView;
}

/** The phone owns the time: wall clock with its own offset, like the old page. */
function localIso(): string {
  const now = new Date();
  const pad = (value: number): string => String(value).padStart(2, '0');
  const offset = -now.getTimezoneOffset();
  const sign = offset >= 0 ? '+' : '-';
  const abs = Math.abs(offset);
  return (
    now.getFullYear() +
    '-' +
    pad(now.getMonth() + 1) +
    '-' +
    pad(now.getDate()) +
    'T' +
    pad(now.getHours()) +
    ':' +
    pad(now.getMinutes()) +
    ':' +
    pad(now.getSeconds()) +
    sign +
    pad(Math.floor(abs / 60)) +
    ':' +
    pad(abs % 60)
  );
}

/** Is the page running as the installed app? */
function runningAsApp(): boolean {
  try {
    return (
      window.matchMedia('(display-mode: standalone)').matches ||
      window.matchMedia('(display-mode: minimal-ui)').matches ||
      window.matchMedia('(display-mode: fullscreen)').matches ||
      window.navigator.standalone === true
    );
  } catch {
    return false;
  }
}

function wakeLockApi(): WakeLockApi | null {
  const value = (navigator as Navigator & { wakeLock?: WakeLockApi }).wakeLock;
  return value ?? null;
}

class Page {
  // ------------------------------------------------------------ what is shown

  mode = $state<PageMode>('boot');
  pairError = $state('');
  version = $state('');
  langCode = $state(language());
  theme = $state<ThemeMode>('dark');

  screen = $state<Screen>('list');
  archived = $state(false);
  monolog = $state<MonologRef | null>(null);

  notes = $state<Note[]>([]);
  sheet = $state<SheetData | null>(null);

  recording = $state<RecordingView | null>(null);
  elapsed = $state(0);

  /** The computer answered the last request; the microphone colour follows it. */
  reachable = $state(true);
  paired = $state<boolean | null>(null);
  apiVersion = $state(0);

  canInstall = $state(false);
  /** Live loudness while recording, 0..1: the rings around the button ride it. */
  level = $state(0);
  /** A new build is installed and waiting: reloading now would lose a take. */
  updateReady = $state(false);
  installed = $state(runningAsApp());

  // ------------------------------------------------------------ loaded windows

  private openList = $state<ListData>(emptyList());
  private archiveList = $state<ListData>(emptyList());
  feed = $state<FeedData | null>(null);

  // ------------------------------------------------------------ the data layer

  private own: Own | null = null;
  private audio: ActiveAudio | null = null;
  private loop: number | null = null;
  private started = false;
  private noteId = 0;
  private offlineNotedAt = 0;
  private installPrompt: InstallPrompt | null = null;
  private traces: string[] = [];

  // ------------------------------------------------------------ read accessors

  /** The list view on screen: open monologs or the archive. */
  get list(): ListData {
    return this.archived ? this.archiveList : this.openList;
  }

  /**
   * A translated string. Reading the language here is what makes a language
   * switch redraw every screen, since the catalogs themselves are not reactive.
   */
  tr(key: string): string {
    void this.langCode;
    return t(key);
  }

  private get cache(): Cache {
    if (!this.own) throw new Error('the page is not set up yet');
    return this.own.cache;
  }

  private get outbox(): Outbox {
    if (!this.own) throw new Error('the page is not set up yet');
    return this.own.outbox;
  }

  private get mutations(): MutationQueue {
    if (!this.own) throw new Error('the page is not set up yet');
    return this.own.mutations;
  }

  private get sync(): Sync {
    if (!this.own) throw new Error('the page is not set up yet');
    return this.own.sync;
  }

  private get api(): Api {
    if (!this.own) throw new Error('the page is not set up yet');
    return this.own.api;
  }

  // ------------------------------------------------------------ boot

  /** Bring the page up. Called once, by `App`, after language and version. */
  async boot(): Promise<void> {
    if (this.started) return;
    this.started = true;
    this.langCode = language();
    // `main.ts` read `version.json` and the saved theme before mounting, so the
    // header and the colours are right in the first paint.
    this.version = pageVersion();
    this.theme = readTheme();
    this.applyDocumentLanguage();

    if (!readToken()) {
      this.showPairing();
      return;
    }

    try {
      const storage = await openStorage();
      const cache = createCache({
        monologs: storage.monologs,
        messages: storage.messages,
        local: storage.local,
        now: localIso,
      });
      const outbox = createOutbox({ store: storage.store, chunks: storage.chunks, now: localIso });
      const mutations = createMutations({ store: storage.mutations });
      // A page that was closed or killed mid-take leaves an open take behind.
      // The recording is over (there is no microphone now): close it so the
      // phone sends `finish` and the whole audio is transcribed.
      for (const take of await outbox.list()) {
        if (!take.closed) await outbox.put({ ...take, closed: true });
      }
      const api = this.createApiLayer();
      const sync = createSync({
        api,
        outbox,
        mutations,
        cache,
        render: (page, pending) => this.render(page, pending),
        renderList: (page, view) => this.renderList(page, view),
        mapped: (localId, serverId) => this.mapped(localId, serverId),
        log: (message) => this.log(message),
        pageSize: FEED_PAGE,
        listPageSize: LIST_PAGE,
      });
      this.own = { storage, cache, outbox, mutations, sync, api };
    } catch (error) {
      this.mode = 'pair';
      this.pairError = this.reason(error);
      return;
    }

    this.mode = 'app';
    this.hookWindow();
    this.sync.archive(false);
    this.sync.show(0);
    // The cache is on screen before anything is asked of the computer.
    await this.reloadList();
    window.app = this.handle();
    this.startLoop();
    void this.kick();
    void this.refreshMe();
  }

  /** The api with the two behaviours the data layer deliberately leaves out. */
  private createApiLayer(): Api {
    const call = createApi({ base: '/site/api', token: () => readToken() });
    return async (method, path, body) => {
      try {
        const answer = await call(method, path, body);
        this.reachable = true;
        return answer;
      } catch (error) {
        if (error instanceof PairingError) {
          // The token is gone or no longer accepted: nothing can be sent until
          // the phone is paired again, and the pairing screen says so.
          this.showPairing();
        } else if (error instanceof OfflineError) {
          this.reachable = false;
        } else {
          // The computer answered and said no: it is reachable.
          this.reachable = true;
        }
        throw error;
      }
    };
  }

  private showPairing(): void {
    this.mode = 'pair';
    this.pairError = this.tr('pair_required');
  }

  private applyDocumentLanguage(): void {
    document.documentElement.lang = this.langCode;
    const manifest = document.getElementById('manifest');
    if (manifest instanceof HTMLLinkElement) {
      manifest.href = `./manifest.${this.langCode}.webmanifest`;
    }
  }

  /** The one handle browser tests and the diagnostics read. */
  private handle(): Window['app'] {
    const page = this;
    const own = this.own;
    if (!own) throw new Error('the page is not set up yet');
    return {
      cache: own.cache,
      outbox: own.outbox,
      mutations: own.mutations,
      sync: own.sync,
      loop: { kick: () => void page.kick() },
      get monolog(): MonologRef | null {
        return page.monolog;
      },
      get archived(): boolean {
        return page.archived;
      },
      get screen(): Screen {
        return page.screen;
      },
      get trace(): readonly string[] {
        return page.traces;
      },
    };
  }

  // ------------------------------------------------------------ global hooks

  private hookWindow(): void {
    window.addEventListener('beforeinstallprompt', (event) => {
      event.preventDefault();
      this.installPrompt = event as InstallPrompt;
      this.canInstall = true;
    });
    window.addEventListener('appinstalled', () => {
      this.installed = true;
      this.installPrompt = null;
      this.canInstall = false;
    });
    window.addEventListener('offline', () => this.toast(this.tr('offline')));
    window.addEventListener('online', () => {
      this.reachable = true;
      void this.kick();
    });
    document.addEventListener('visibilitychange', () => {
      // A phone whose screen went dark comes back here: send what waited.
      if (!document.hidden) void this.kick();
    });
    window.addEventListener('popstate', () => this.onPopState());
  }

  private onPopState(): void {
    // One rule: the browser says which screen we are back on, and the page
    // follows it. A sheet is closed first and the screen underneath stays.
    if (this.sheet) {
      this.sheet = null;
      return;
    }
    const state = historyView();
    if (state?.view === 'monolog') return;
    if (state?.view === 'archive') {
      this.archived = true;
      if (this.monolog) this.leaveMonolog();
      else {
        this.sync.archive(true);
        void this.reloadList();
      }
      return;
    }
    if (this.monolog) this.leaveMonolog();
    else if (this.archived) this.leaveArchive();
  }

  // ------------------------------------------------------------ navigation

  /** What back closes, shared by the header arrow and the phone's gesture. */
  back(): void {
    if (this.sheet) {
      this.dismissSheet();
      return;
    }
    const state = historyView();
    if (state && state.view && state.view !== 'list') {
      history.back();
      return;
    }
    if (this.monolog) this.leaveMonolog();
    else if (this.archived) this.leaveArchive();
  }

  async openMonolog(input: { id: number; name: string; local?: boolean }): Promise<void> {
    const id = Number(input.id);
    if (!Number.isFinite(id)) return;
    this.monolog = { id, name: input.name, local: input.local === true };
    this.screen = 'feed';
    this.sync.show(id);
    this.sync.archive(this.archived);
    const feed: FeedData = {
      loaded: [],
      pending: [],
      total: 0,
      hasOlder: false,
      loadingOlder: false,
      scrollTick: 0,
      atBottom: true,
    };
    this.feed = feed;
    this.pushView({ view: 'monolog', id });
    // Cached rows and the waiting takes are on screen before the computer is
    // asked, so opening a monolog offline is never a blank wait.
    feed.pending = (await this.outbox.list()).filter(
      (take) => Number(take.monolog) === id && !take.remote,
    );
    const page = await this.cache.messagePage(id, { limit: FEED_PAGE });
    feed.loaded = page.messages;
    feed.total = Math.max(feed.loaded.length, await this.cache.messageCount(id));
    feed.hasOlder = feed.loaded.length < feed.total || page.hasMore === true;
    feed.scrollTick += 1;
    void this.kick();
  }

  leaveMonolog(): void {
    this.monolog = null;
    this.feed = null;
    this.screen = 'list';
    this.sync.show(0);
    this.sync.archive(this.archived);
    void this.reloadList();
    void this.kick();
  }

  openArchive(): void {
    if (this.archived) return;
    this.archived = true;
    this.sync.archive(true);
    this.pushView({ view: 'archive' });
    void this.reloadList();
    void this.kick();
  }

  leaveArchive(): void {
    this.archived = false;
    this.sync.archive(false);
    void this.reloadList();
    void this.kick();
  }

  /** The recording continues on every screen; this bar brings the person back. */
  openRecordingMonolog(): void {
    const recording = this.recording;
    if (!recording) return;
    void this.openMonolog({ id: recording.monologId, name: recording.name });
  }

  private pushView(view: HistoryView): void {
    try {
      history.pushState(view, '');
    } catch {
      // A browser that refuses history still works; back just leaves the app.
    }
  }

  // ------------------------------------------------------------ sheets

  openCreate(): void {
    this.pushSheet({
      kind: 'create',
      title: this.tr('new_monolog'),
      hint: '',
      field: '',
      placeholder: this.tr('monolog_name'),
      monolog: null,
      message: null,
      archivedView: false,
    });
  }

  /** The header's ⋯ on the open monolog. */
  openMenu(): void {
    const monolog = this.findOpenMonolog();
    if (monolog) this.openCardMenu(monolog);
  }

  /** A card's ⋯: the same sheet, opened without entering the monolog. */
  openCardMenu(monolog: Monolog): void {
    this.pushSheet({
      kind: 'menu',
      title: monolog.name,
      hint: this.archived ? this.tr('archive_read_only') : '',
      field: this.archived ? null : monolog.name,
      placeholder: '',
      monolog,
      message: null,
      archivedView: this.archived,
    });
  }

  openEdit(message: Message): void {
    this.pushSheet({
      kind: 'edit',
      title: this.tr('edit_title'),
      hint: this.tr('edit_hint'),
      field: textOf(message),
      placeholder: '',
      monolog: null,
      message,
      archivedView: this.archived,
    });
  }

  openDelete(message: Message): void {
    this.pushSheet({
      kind: 'delete',
      title: this.tr('delete_title'),
      hint: this.tr('delete_hint'),
      field: null,
      placeholder: '',
      monolog: null,
      message,
      archivedView: this.archived,
    });
  }

  private pushSheet(sheet: SheetData): void {
    this.sheet = sheet;
    this.pushView({ view: 'sheet' });
  }

  /**
   * Close the sheet the way its own buttons do. The history entry it pushed is
   * spent, so the browser and the page stay in step.
   */
  dismissSheet(): void {
    if (historyView()?.view === 'sheet') {
      history.back();
      return;
    }
    this.sheet = null;
  }

  /** The primary button of whichever sheet is open. */
  async submitSheet(text: string): Promise<void> {
    const sheet = this.sheet;
    if (!sheet) return;
    const value = text.trim();
    if (sheet.kind === 'create') {
      if (!value) return;
      this.dismissSheet();
      await this.createMonolog(value);
      return;
    }
    if (sheet.kind === 'menu') {
      const monolog = sheet.monolog;
      this.dismissSheet();
      if (monolog && value && value !== monolog.name) await this.renameMonolog(monolog, value);
      return;
    }
    if (sheet.kind === 'edit') {
      const message = sheet.message;
      this.dismissSheet();
      if (message && value && value !== textOf(message)) await this.editMessage(message, value);
      return;
    }
    if (sheet.kind === 'delete') {
      const message = sheet.message;
      this.dismissSheet();
      if (message) await this.deleteMessage(message);
    }
  }

  /** Archive (from the open list) or unarchive (from the archive) one monolog. */
  async archiveFromSheet(): Promise<void> {
    const sheet = this.sheet;
    if (!sheet || sheet.kind !== 'menu') return;
    const monolog = sheet.monolog;
    const archived = !sheet.archivedView;
    const wasOpen = monolog !== null && this.monolog !== null && this.monolog.id === monolog.id;
    this.dismissSheet();
    if (!monolog) return;
    await this.setArchived(monolog, archived);
    if (wasOpen) {
      // The open monolog is no longer in the list it belongs to: leave the chat
      // and let the list (without it) be what back lands on.
      this.archived = false;
      this.monolog = null;
      this.feed = null;
      this.screen = 'list';
      this.sync.show(0);
      this.sync.archive(false);
      if (historyView()?.view === 'monolog') history.back();
      await this.reloadList();
      void this.kick();
    }
  }

  // ------------------------------------------------------------ manual changes
  //
  // The same four steps in every one of them: the cache takes the change, the
  // queue takes it, the view is redrawn from the cache, and the sync loop is
  // asked to run. The network is never awaited here.

  async createMonolog(name: string): Promise<void> {
    // A monolog is made on the phone first: it gets a negative id, shows up in
    // the list at once, and the sync loop creates it on the computer.
    await this.cache.allocate(name);
    await this.reloadList();
    this.sync.archive(false);
    void this.kick();
  }

  async renameMonolog(monolog: Monolog, name: string): Promise<void> {
    await this.cache.applyRename(monolog, name);
    await this.mutations.put({
      kind: 'monolog.rename',
      target: { monolog: monolog.id },
      payload: { name },
      local: Number(monolog.id) < 0,
    });
    if (this.monolog && this.monolog.id === Number(monolog.id)) {
      this.monolog = { ...this.monolog, name };
    }
    if (this.recording && this.recording.monologId === Number(monolog.id)) {
      this.recording = { ...this.recording, name };
    }
    await this.reloadList();
    void this.kick();
  }

  async setArchived(monolog: Monolog, archived: boolean): Promise<void> {
    await this.cache.applyArchive(monolog, archived);
    await this.mutations.put({
      kind: 'monolog.archive',
      target: { monolog: monolog.id },
      payload: { archived },
      local: Number(monolog.id) < 0,
    });
    await this.reloadList();
    void this.kick();
  }

  async editMessage(message: Message, text: string): Promise<void> {
    const monologId = Number(message.monolog);
    await this.cache.applyEdit(message, text);
    await this.mutations.put({
      kind: 'message.edit',
      target: { monolog: monologId, message: message.id },
      payload: { text },
      local: monologId < 0,
    });
    // The cache decides what the feed shows: it lays the override over the row.
    const accepted = await this.cache.message(monologId, message.id);
    if (accepted) this.replaceLoaded(accepted);
    void this.kick();
  }

  async deleteMessage(message: Message): Promise<void> {
    const monologId = Number(message.monolog);
    await this.cache.applyDelete(message);
    await this.mutations.put({
      kind: 'message.delete',
      target: { monolog: monologId, message: message.id },
      payload: {},
      local: monologId < 0,
    });
    // A tombstone: the cache read answers `null`, so the row leaves the feed.
    const gone = await this.cache.message(monologId, message.id);
    if (gone === null) this.dropLoaded(message.id);
    void this.kick();
  }

  /** Throw away a take that never left the phone, or one that never finished. */
  async discardTake(take: Take): Promise<void> {
    if (this.recording && this.recording.take.localId === take.localId) {
      await this.stopRecording();
    }
    if (take.remote) {
      const remote = Number(take.remote);
      await this.cache.applyDelete({ id: remote });
      await this.mutations.put({
        kind: 'message.delete',
        target: { monolog: Number(take.monolog), message: remote },
        payload: {},
        local: Number(take.monolog) < 0,
      });
      this.dropLoaded(remote);
    }
    await this.outbox.remove(take.localId);
    await this.refreshFeed();
    void this.kick();
  }

  async retryMessage(message: Message): Promise<void> {
    try {
      await this.api('POST', '/messages/' + String(message.id) + '/retry');
      void this.kick();
    } catch (error) {
      this.toast(this.reason(error));
    }
  }

  async copyText(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      this.toast(this.tr('copied'));
    } catch {
      this.toast(this.tr('copy_failed'));
    }
  }

  // ------------------------------------------------------------ the feed window

  private replaceLoaded(message: Message): void {
    const feed = this.feed;
    if (!feed) return;
    const index = feed.loaded.findIndex((row) => messageId(row) === messageId(message));
    if (index < 0) return;
    feed.loaded = feed.loaded.map((row, at) => (at === index ? message : row));
  }

  private dropLoaded(id: Message['id']): void {
    const feed = this.feed;
    if (!feed) return;
    const number = Number(id);
    feed.loaded = feed.loaded.filter((row) => messageId(row) !== number);
    feed.total = Math.max(feed.loaded.length, feed.total - 1);
    feed.hasOlder = feed.loaded.length < feed.total;
  }

  /** Re-read the open feed from the cache; used after a take is dropped. */
  private async refreshFeed(): Promise<void> {
    const feed = this.feed;
    const monolog = this.monolog;
    if (!feed || !monolog) return;
    const limit = Math.max(FEED_PAGE, Math.min(FEED_MAX, feed.loaded.length));
    const page = await this.cache.messagePage(monolog.id, { limit });
    feed.loaded = page.messages;
    feed.pending = (await this.outbox.list()).filter(
      (take) => Number(take.monolog) === monolog.id && !take.remote,
    );
    feed.total = Math.max(feed.loaded.length, await this.cache.messageCount(monolog.id));
    feed.hasOlder = feed.loaded.length < feed.total || page.hasMore === true;
  }

  /**
   * Draw the newest page the sync loop fetched. A redraw keeps the reader's
   * place; only a reader already at the bottom follows new messages.
   */
  private render(page: MessagePage, pending: Take[]): void {
    const feed = this.feed;
    const monolog = this.monolog;
    if (!feed || !monolog) return;
    const wasAtBottom = feed.atBottom;
    const merged = new Map<number, Message>();
    for (const row of feed.loaded) merged.set(messageId(row), row);
    for (const row of page.messages) merged.set(messageId(row), row);
    feed.loaded = [...merged.values()].sort((left, right) => {
      const a = String(left.recorded || '');
      const b = String(right.recorded || '');
      if (a !== b) return a < b ? -1 : 1;
      return messageId(left) - messageId(right);
    });
    if (page.total !== null) feed.total = Math.max(page.total, feed.loaded.length);
    feed.total = Math.max(feed.total, feed.loaded.length);
    feed.hasOlder = page.hasMore === true || feed.loaded.length < feed.total;
    feed.pending = pending.filter(
      (take) => Number(take.monolog) === monolog.id && !take.remote,
    );
    this.trimFeed(wasAtBottom);
    feed.atBottom = wasAtBottom;
    if (wasAtBottom) feed.scrollTick += 1;
  }

  /** Keep one window in memory: the newest end for a reader at the bottom. */
  private trimFeed(fromBottom: boolean): void {
    const feed = this.feed;
    if (!feed || feed.loaded.length <= FEED_MAX) return;
    if (fromBottom) {
      feed.loaded = feed.loaded.slice(feed.loaded.length - FEED_MAX);
      feed.hasOlder = true;
    } else {
      feed.loaded = feed.loaded.slice(0, FEED_MAX);
      feed.total = Math.max(feed.total, feed.loaded.length);
    }
  }

  /** One older page, cache first; the reader's place is kept across the insert. */
  async loadOlder(): Promise<void> {
    const feed = this.feed;
    const monolog = this.monolog;
    if (!feed || !monolog || feed.loadingOlder || !feed.hasOlder) return;
    feed.loadingOlder = true;
    const view = viewNode();
    const beforeTop = view?.scrollTop ?? 0;
    const beforeHeight = view?.scrollHeight ?? 0;
    try {
      const first = feed.loaded.at(0) ?? null;
      const page = await this.sync.page(monolog.id, {
        before: first ? { id: first.id, recorded: first.recorded } : null,
        limit: FEED_PAGE,
      });
      if (this.feed !== feed) return;
      if (page.total !== null) feed.total = Math.max(feed.total, page.total);
      const seen = new Set(feed.loaded.map((row) => messageId(row)));
      const older = page.messages.filter((row) => !seen.has(messageId(row)));
      if (older.length > 0) {
        feed.loaded = [...older, ...feed.loaded].sort((left, right) => {
          const a = String(left.recorded || '');
          const b = String(right.recorded || '');
          if (a !== b) return a < b ? -1 : 1;
          return messageId(left) - messageId(right);
        });
        feed.hasOlder = page.hasMore === true || older.length >= FEED_PAGE;
        if (feed.loaded.length > FEED_MAX) {
          feed.loaded = feed.loaded.slice(feed.loaded.length - FEED_MAX);
        }
      } else {
        feed.hasOlder = page.hasMore === true;
      }
    } catch (error) {
      this.log('older page failed: ' + this.reason(error));
    } finally {
      feed.loadingOlder = false;
    }
    if (view && feed.loaded.length > 0) {
      // The rows above the reader grew: put the reader back where they were.
      await tick();
      view.scrollTop = beforeTop + (view.scrollHeight - beforeHeight);
    }
  }

  jumpToNewest(): void {
    const feed = this.feed;
    if (!feed) return;
    feed.atBottom = true;
    feed.scrollTick += 1;
  }

  /** Called by the scroll container for every scroll event. */
  handleScroll(): void {
    const view = viewNode();
    if (!view) return;
    const feed = this.feed;
    if (feed) {
      feed.atBottom = view.scrollTop + view.clientHeight >= view.scrollHeight - 48;
      if (view.scrollTop < LOAD_MORE_PX && feed.hasOlder && !feed.loadingOlder) {
        void this.loadOlder();
      }
      return;
    }
    const below = view.scrollHeight - (view.scrollTop + view.clientHeight);
    if (below < LOAD_MORE_PX) void this.loadMoreList();
  }

  // ------------------------------------------------------------ the list window

  private async reloadList(): Promise<void> {
    if (!this.own) return;
    const list = this.list;
    const count = Math.max(LIST_PAGE, Math.min(LIST_MAX, list.records.length));
    const page = await this.cache.monologPage(this.archived, 0, count);
    list.records = page.monologs;
    const localCount = this.archived ? 0 : (await this.cache.localMonologs()).length;
    const serverTotal = this.sync.monologTotal(this.archived);
    list.total = Math.max(
      page.total,
      serverTotal === null ? 0 : serverTotal + localCount,
      list.records.length,
    );
    list.pending = await this.outbox.list();
  }

  private appendRecords(list: ListData, rows: readonly Monolog[]): number {
    const seen = new Set(list.records.map((row) => Number(row.id)));
    let added = 0;
    for (const row of rows) {
      const id = Number(row.id);
      if (!Number.isFinite(id) || seen.has(id)) continue;
      seen.add(id);
      list.records = [...list.records, row];
      added += 1;
    }
    return added;
  }

  async loadMoreList(): Promise<void> {
    if (!this.own) return;
    const list = this.list;
    if (list.loading) return;
    if (list.records.length >= list.total || list.records.length >= LIST_MAX) return;
    list.loading = true;
    try {
      let page = await this.cache.monologPage(this.archived, list.records.length, LIST_PAGE);
      let added = this.appendRecords(list, page.monologs);
      if (added === 0 && list.records.length < list.total) {
        const cachedServer = await this.cache.serverMonologCount(this.archived);
        const answer = await this.sync.fetchMonologPage(
          this.archived,
          cachedServer,
          LIST_PAGE,
        );
        if (answer) {
          const localCount = this.archived ? 0 : (await this.cache.localMonologs()).length;
          if (answer.total !== null) {
            list.total = Math.max(list.total, answer.total + localCount);
          }
          page = await this.cache.monologPage(this.archived, list.records.length, LIST_PAGE);
          added = this.appendRecords(list, page.monologs);
          if (!answer.hasMore && added === 0) list.total = list.records.length;
        } else {
          // Offline: whatever is stored is the whole list there is.
          list.total = list.records.length;
        }
      }
      if (added === 0 && page.total <= list.records.length) {
        list.total = list.records.length;
      }
      list.pending = await this.outbox.list();
    } finally {
      list.loading = false;
    }
  }

  /** The sync loop fetched a list page; only the view on screen is redrawn. */
  private async renderList(_page: ListResult, archivedView: boolean): Promise<void> {
    if (this.mode !== 'app' || this.monolog || this.screen !== 'list') return;
    if (this.archived !== archivedView) return;
    await this.reloadList();
  }

  /** A local monolog got its server id under the reader. */
  private mapped(localId: number, serverId: number): void {
    const local = Number(localId);
    const remote = Number(serverId);
    if (this.recording && this.recording.monologId === local) {
      this.recording = { ...this.recording, monologId: remote };
    }
    if (!this.monolog || this.monolog.id !== local) return;
    this.monolog = { ...this.monolog, id: remote, local: false };
    this.feed = {
      loaded: [],
      pending: [],
      total: 0,
      hasOlder: false,
      loadingOlder: false,
      scrollTick: 0,
      atBottom: true,
    };
    this.sync.show(remote);
    try {
      history.replaceState({ view: 'monolog', id: remote }, '');
    } catch {
      // A browser without history still records.
    }
    void this.refreshFeed();
  }

  private findOpenMonolog(): Monolog | null {
    const current = this.monolog;
    if (!current) return null;
    const found = this.list.records.find((row) => Number(row.id) === current.id);
    if (found) return found;
    return {
      id: current.id,
      name: current.name,
      local: current.local,
    };
  }

  // ------------------------------------------------------------ recording

  /** The round button: start when idle, stop when a take is running. */
  async toggleRecording(): Promise<void> {
    if (this.recording) await this.stopRecording();
    else await this.startRecording();
  }

  /**
   * Microphone → 16 kHz mono PCM16 → 0.5 s chunks → IndexedDB → the sync loop.
   * Every chunk is stored before it is sent, so a lost connection only delays
   * the text; it never loses the words.
   */
  async startRecording(): Promise<void> {
    const monolog = this.monolog;
    if (this.recording || !monolog || this.archived) return;
    const take = this.outbox.newTake(monolog.id);
    await this.outbox.put(take);

    let stream: MediaStream;
    try {
      stream = await navigator.mediaDevices.getUserMedia({
        audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true },
      });
    } catch {
      await this.outbox.remove(take.localId);
      this.toast(this.tr('mic_denied'));
      return;
    }

    const context = new AudioContext();
    await context.resume();
    let wakeLock: WakeLockLike | null = null;
    const locks = wakeLockApi();
    if (locks) {
      try {
        wakeLock = await locks.request('screen');
      } catch {
        // The screen may still sleep; the recording survives in IndexedDB.
      }
    }

    const source = context.createMediaStreamSource(stream);
    // The rings follow a cleaned level: a fridge or a fan must not make the button
    // ripple while nobody speaks. The analyser listens to the same graph the
    // recording comes from, so nothing extra is captured.
    const analyser = context.createAnalyser();
    analyser.fftSize = 2048;
    analyser.smoothingTimeConstant = 0.2;
    source.connect(analyser);
    const spectrum = new Float32Array(analyser.frequencyBinCount);
    const waveform = new Float32Array(analyser.fftSize);
    const cleaner = new NoiseCleaner(context.sampleRate, analyser.fftSize);
    const meter = window.setInterval(() => {
      analyser.getFloatFrequencyData(spectrum);
      analyser.getFloatTimeDomainData(waveform);
      let energy = 0;
      for (let index = 0; index < waveform.length; index += 1) {
        const value = waveform[index] ?? 0;
        energy += value * value;
      }
      const rms = Math.sqrt(energy / Math.max(1, waveform.length));
      const db = 20 * Math.log10(rms + 1e-8);
      this.level = cleaner.measure(spectrum, db, METER_SECONDS);
    }, METER_MS);

    const processor = context.createScriptProcessor(4096, 1, 1);
    const ratio = context.sampleRate / RECORD_RATE;
    let carry: number[] = [];

    const enqueue = (samples: number[]): void => {
      const buffer = new ArrayBuffer(samples.length * 2);
      const view = new Int16Array(buffer);
      for (let index = 0; index < samples.length; index += 1) {
        const value = Math.max(-1, Math.min(1, samples[index] ?? 0));
        view[index] = value < 0 ? value * 0x8000 : value * 0x7fff;
      }
      const seq = take.seq;
      take.seq = seq + 1;
      void this.outbox
        .putChunk(take.localId, seq, new Uint8Array(buffer))
        .then(() => this.outbox.put(take))
        .then(() => this.kick())
        .catch(() => this.log('a chunk could not be stored'));
    };

    processor.onaudioprocess = (event: AudioProcessingEvent) => {
      const input = event.inputBuffer.getChannelData(0);
      for (let index = 0; index < input.length; index += ratio) {
        let sum = 0;
        let count = 0;
        for (let inner = 0; inner < ratio && index + inner < input.length; inner += 1) {
          sum += input[index + inner] ?? 0;
          count += 1;
        }
        carry.push(count ? sum / count : 0);
      }
      while (carry.length >= CHUNK_SAMPLES) {
        const slice = carry.slice(0, CHUNK_SAMPLES);
        carry = carry.slice(CHUNK_SAMPLES);
        enqueue(slice);
      }
    };
    source.connect(processor);
    processor.connect(context.destination);

    const stopMeter = (): void => window.clearInterval(meter);

    const tail = (): void => {
      stopMeter();
      if (carry.length > 0) {
        enqueue(carry);
        carry = [];
      }
    };
    this.audio = { take, context, stream, processor, source, wakeLock, tail };
    this.recording = {
      take,
      monologId: monolog.id,
      name: monolog.name,
      started: Date.now(),
    };
    this.elapsed = 0;
    void this.kick();
  }

  async stopRecording(): Promise<void> {
    const audio = this.audio;
    // The button stops looking like a recording before any slow cleanup: a
    // failure below must never leave it red forever.
    this.level = 0;
    this.audio = null;
    this.recording = null;
    this.elapsed = 0;
    if (!audio) return;
    try {
      audio.processor.disconnect();
      audio.source.disconnect();
      for (const track of audio.stream.getTracks()) track.stop();
      await audio.context.close();
    } catch {
      // Closing a closed context is not an error worth reporting.
    }
    if (audio.wakeLock) {
      try {
        await audio.wakeLock.release();
      } catch {
        // Released automatically when the page is hidden.
      }
    }
    try {
      audio.tail();
    } catch {
      this.log('the last audio of the take could not be stored');
    }
    audio.take.closed = true;
    try {
      await this.outbox.put(audio.take);
    } catch {
      this.log('the stopped take could not be stored');
    }
    void this.kick();
  }

  /** Called by the composer's clock while a take is running. */
  tickElapsed(): void {
    const recording = this.recording;
    if (!recording) return;
    this.elapsed = Math.floor((Date.now() - recording.started) / 1000);
  }

  // ------------------------------------------------------------ the sync loop

  /** Ask the loop to run now. Never awaited by a click handler. */
  async kick(): Promise<void> {
    if (!this.own) return;
    let result: TickResult;
    try {
      result = await this.sync.tick();
    } catch (error) {
      this.log('tick failed: ' + this.reason(error));
      return;
    }
    if (result.refused && result.refused.length > 0) this.reportRefusals(result.refused);
    if (result.failed && !this.reachable) void this.offlineNoteIfWork();
  }

  /**
   * The one repeating timer. It goes through `kick` rather than `sync.start`,
   * so an interval tick reports a refusal and says the computer is away exactly
   * like a tick after an action does.
   */
  private startLoop(intervalMs = 1500): void {
    if (this.loop !== null) return;
    this.loop = window.setInterval(() => {
      void this.kick();
    }, intervalMs);
  }

  private reportRefusals(refused: readonly MutationFailure[]): void {
    for (const failure of refused) {
      this.toast(failure.reason);
      this.log('mutation ' + failure.seq + ' refused: ' + failure.reason);
    }
  }

  /**
   * The computer is away. Say it only when there is work that cannot go out:
   * an idle phone with nothing queued is not a problem worth a banner.
   */
  private async offlineNoteIfWork(): Promise<void> {
    if (!this.own) return;
    const takes = await this.outbox.list();
    const queued = await this.mutations.count();
    if (this.recording || takes.length > 0 || queued > 0) this.offlineNote();
  }

  private log(message: string): void {
    // A phone has no console. The trace is for diagnostics, not for the user.
    this.traces.push(message);
    if (this.traces.length > 200) this.traces.shift();
  }

  // ------------------------------------------------------------ language, notes

  async changeLanguage(code: string): Promise<void> {
    const chosen = await setLanguage(code);
    this.langCode = chosen;
    this.applyDocumentLanguage();
  }

  /** Switch the page between its two modes; the class does the rest. */
  setTheme(mode: ThemeMode): void {
    this.theme = mode;
    applyTheme(mode);
  }

  async refreshMe(): Promise<void> {
    if (!this.own) return;
    try {
      const answer = await this.api('GET', '/me');
      this.paired = answer.paired === true;
      const api = Number(answer.api);
      if (Number.isFinite(api)) this.apiVersion = api;
    } catch {
      // No answer is not "unpaired": leave the last known state alone.
    }
  }

  /** The reader pressed the update banner: a new build is installed. */
  applyUpdate(): void {
    location.reload();
  }

  /** Told by the update watcher that a reload would interrupt a take. */
  markUpdateReady(): void {
    this.updateReady = true;
  }

  toast(text: string): void {
    const id = ++this.noteId;
    this.notes = [...this.notes, { id, text }];
    window.setTimeout(() => {
      this.notes = this.notes.filter((note) => note.id !== id);
    }, 2200);
  }

  /** The same failure repeats every tick; one note says it better than ten. */
  offlineNote(): void {
    const now = Date.now();
    if (now - this.offlineNotedAt < 5000) return;
    this.offlineNotedAt = now;
    this.toast(this.tr('offline_recording'));
  }

  /** Why a request failed, in the person's language where a key exists. */
  private reason(error: unknown): string {
    if (error instanceof OfflineError) return this.tr('offline_long');
    if (error instanceof PairingError) return this.tr('pair_required');
    const message = error instanceof Error ? error.message : String(error);
    if (message.includes('local network')) return this.tr('lan_blocked');
    return this.tr('cannot_reach') + (message ? ' · ' + message : '');
  }

  // ------------------------------------------------------------ install

  async promptInstall(): Promise<void> {
    const prompt = this.installPrompt;
    if (!prompt) return;
    this.installPrompt = null;
    this.canInstall = false;
    await prompt.prompt();
  }

  reload(): void {
    location.reload();
  }

  /** The version label the header shows: `v1.17.4`, or empty when unknown. */
  get versionLabel(): string {
    return this.version ? 'v' + this.version : '';
  }
}

declare global {
  interface Navigator {
    /** Safari's flag for a page running from the home screen. */
    standalone?: boolean;
  }

  interface Window {
    app?: {
      cache: Cache;
      outbox: Outbox;
      mutations: MutationQueue;
      sync: Sync;
      loop: { kick: () => void };
      readonly monolog: MonologRef | null;
      readonly archived: boolean;
      readonly screen: Screen;
      readonly trace: readonly string[];
    };
  }
}

/** The one page state. */
export const page = new Page();
