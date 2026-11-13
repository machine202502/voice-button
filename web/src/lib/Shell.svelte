<script lang="ts">
  import ScrollThumb from './ScrollThumb.svelte';

  // The frame every screen lives in: the header, the recording bar, the one
  // scrolling container, the notes, the footer and the composer.
  //
  // The header follows the state, not the screen's props: which controls exist
  // is a property of where the person is (a monolog open? the archive?), and
  // putting that decision in one place is what keeps the arrow, the menu and
  // the archive button from disagreeing with each other.

  import type { Snippet } from 'svelte';
  import { page } from '../state.svelte';
  import { formatDuration } from './time';

  interface Props {
    title: string;
    /** Back leaves the monolog by walking the browser's own history. */
    onback: () => void;
    /** The composer belongs to the monolog tab; the others have their own. */
    composerVisible?: boolean;
    tabs?: Snippet;
    children: Snippet;
    composer?: Snippet;
  }

  let scroller = $state<HTMLElement | null>(null);

  let { title, onback, composerVisible = true, tabs, children, composer }: Props = $props();

  const inFeed = $derived(page.screen === 'feed' && page.monolog !== null);
  const showComposer = $derived(inFeed && !page.archived && composerVisible);
  const recording = $derived(page.recording);
  const barVisible = $derived(
    recording !== null && (page.monolog === null || page.monolog.id !== recording.monologId),
  );
</script>

<div class="vb-shell">
  <header>
    <button
      class="icon-button"
      id="back"
      type="button"
      aria-label={page.tr('back')}
      hidden={!inFeed}
      onclick={onback}>←</button
    >
    <h1 id="title">{title}</h1>
    <span class="vb-grow"></span>
    <span class="version" id="version" aria-label={page.tr('site_version')}
      >{page.versionLabel}</span
    >
    <button
      class="icon-button"
      id="archive"
      type="button"
      aria-label={page.tr('archive')}
      hidden={page.archived || inFeed}
      onclick={() => page.openArchive()}>⌸</button
    >
    <button
      class="icon-button"
      id="menu"
      type="button"
      aria-label={page.tr('monolog')}
      hidden={!inFeed}
      onclick={() => page.openMenu()}>⋯</button
    >
  </header>

  <!-- The recording is not lost when the monolog is left: this bar follows it
       to every screen and brings the person back. -->
  <button
    class="rec-bar"
    id="rec-bar"
    type="button"
    hidden={!barVisible}
    onclick={() => page.openRecordingMonolog()}
  >
    <span class="dot"></span>
    <span id="rec-bar-text"
      >{page.tr('recording')}{recording ? ' · ' + formatDuration(page.elapsed) : ''}</span
    >
    <span class="open">{page.tr('open')}</span>
  </button>

  {#if tabs}
    {@render tabs()}
  {/if}

  <main id="view" class="vb-main" bind:this={scroller} onscroll={() => page.handleScroll()}>
    {@render children()}
  </main>
  <ScrollThumb target={scroller} />

  <!-- Transient messages live in the flow, above the footer: a fixed toast used
       to land on top of the install button on a phone. -->
  <div id="notes" class="notes" role="status" aria-live="polite"
    >{#each page.notes as note (note.id)}<p class="banner">{note.text}</p>{/each}{#if page.updateReady}<p class="banner"><button class="action" id="update-app" type="button" onclick={() => page.applyUpdate()}>{page.tr('update_app')}</button></p>{/if}</div
  >

  <footer class="about" hidden={page.installed}>
    <a class="footer-link" id="to-install" href="/install">{page.tr('install_app')}</a>
    <span class="vb-grow"></span>
    <button
      class="action"
      id="install"
      type="button"
      hidden={!page.canInstall}
      onclick={() => void page.promptInstall()}>{page.tr('install')}</button
    >
  </footer>

  <div id="composer" class="vb-composer" hidden={!showComposer}>
    {#if composer}
      {@render composer()}
    {/if}
  </div>
</div>
