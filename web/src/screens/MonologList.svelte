<script lang="ts">
  // The list itself: the loaded page of monologs and the archive's read-only
  // notice. More pages arrive while the person scrolls (`handleScroll`); the
  // screen keeps a window over the store.

  import MonologCard from '../lib/MonologCard.svelte';
  import { monologPluralKey } from '../lib/format';
  import { page } from '../state.svelte';

  const loaded = $derived(page.list.records.length);
  const total = $derived(page.list.total);
</script>

<div class="vb-screen">
  {#if page.archived}
    <p class="vb-kicker vb-archive-note">
      {total} {page.tr(monologPluralKey(total, page.langCode))} · {page.tr('read_only')}
    </p>
  {/if}

  {#if loaded === 0}
    <div class="empty">{page.tr(page.archived ? 'archive_empty' : 'no_monologs')}</div>
  {/if}

  {#each page.list.records as monolog (monolog.id)}
    <MonologCard {monolog} />
  {/each}
</div>
