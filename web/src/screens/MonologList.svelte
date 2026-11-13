<script lang="ts">
  // The list itself: the loaded page of monologs and the way to make one.
  //
  // The rows are what the cache returned, newest first. More pages arrive while
  // the person scrolls (`handleScroll`), and the list stops at a few hundred
  // rows: the store keeps everything, the screen keeps a window.

  import MonologCard from '../lib/MonologCard.svelte';
  import { page } from '../state.svelte';
</script>

<div class="vb-screen">
  {#if page.list.records.length === 0}
    <div class="empty">{page.tr(page.archived ? 'archive_empty' : 'no_monologs')}</div>
  {/if}

  {#each page.list.records as monolog (monolog.id)}
    <MonologCard {monolog} />
  {/each}

  {#if !page.archived && page.list.records.length >= page.list.total}
    <button class="new-monolog" type="button" onclick={() => page.openCreate()}>
      {page.tr('new_monolog')}
    </button>
  {/if}
</div>
