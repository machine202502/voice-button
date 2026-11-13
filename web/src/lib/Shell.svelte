<script lang="ts">
  // The frame every screen lives in: the header, the one scrolling viewport,
  // the scroll indicator clipped to that viewport, the notes, the install
  // footer and the monolog composer.
  //
  // The header follows the state, not the screen's props: which controls exist
  // is a property of where the person is, and putting that decision in one
  // place keeps the arrow, the menu and the title from disagreeing.

  import type { Snippet } from 'svelte';
  import { page } from '../state.svelte';
  import Composer from './Composer.svelte';
  import RecordingPlate from './RecordingPlate.svelte';
  import ScrollThumb from './ScrollThumb.svelte';

  interface Props {
    title: string;
    children: Snippet;
  }

  let { title, children }: Props = $props();

  let scroller = $state<HTMLElement | null>(null);

  const inFeed = $derived(page.screen === 'feed' && page.monolog !== null);
  const showBack = $derived(inFeed || page.archived || page.screen === 'settings');
  const showMenu = $derived(!page.selection && (inFeed || page.screen === 'list'));
  const showComposer = $derived(inFeed && !page.archived);
  const selectedCount = $derived(page.selected.length);
</script>

<div class="vb-shell">
  <header>
    <button
      class="icon-button"
      id="back"
      type="button"
      aria-label={page.tr('back')}
      hidden={!showBack}
      onclick={() => page.back()}>←</button
    >
    <h1 id="title">{title}</h1>
    <span class="vb-grow"></span>
    <span class="version" id="version" aria-label={page.tr('site_version')}
      >{page.versionLabel}</span
    >
    <button
      class="icon-button"
      id="menu"
      type="button"
      aria-label={page.tr('more')}
      hidden={!showMenu}
      onclick={() => page.openHeaderMenu()}>⋯</button
    >
  </header>

  <div class="vb-viewport">
    <main id="view" class="vb-main" bind:this={scroller} onscroll={() => page.handleScroll()}>
      {@render children()}
    </main>
    <ScrollThumb target={scroller} />
  </div>

  <div class="vb-dock">
    {#if page.selection}
      <div class="vb-select-bar" role="toolbar" aria-label={page.tr('select_several')}>
        <span class="vb-select-count">{selectedCount} {page.tr('selected')}</span>
        <button
          class="vb-select-action"
          type="button"
          disabled={selectedCount === 0}
          onclick={() => void page.archiveSelection()}
        >
          {page.tr(page.archived ? 'restore' : 'to_archive')}
        </button>
        <button class="vb-select-cancel" type="button" onclick={() => page.cancelSelection()}>
          {page.tr('cancel')}
        </button>
      </div>
    {/if}

    {#if page.recording}
      <RecordingPlate />
    {/if}

    <!-- Transient messages are an overlay above the composer area, never a row
         in the layout: a note must not push the content or move the reader. -->
    <div
      id="notes"
      class="notes"
      class:with-plate={page.recording !== null}
      role="status"
      aria-live="polite"
      >{#each page.notes as note (note.id)}<p class="banner">{note.text}</p>{/each}{#if page.updateReady}<p class="banner"><button
            class="action"
            id="update-app"
            type="button"
            onclick={() => page.applyUpdate()}>{page.tr('update_app')}</button
          ></p>{/if}</div
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
      {#if showComposer}
        <Composer />
      {/if}
    </div>
  </div>
</div>
