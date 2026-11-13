<script lang="ts">
  // One monolog as a chat: oldest on top, newest at the bottom, one divider per
  // day, and the takes that are still only on the phone after the newest row.
  //
  // The list follows the newest row only while the reader is at the bottom; a
  // reader who scrolled up keeps their place while older pages are prepended.

  import MessageCard from '../lib/MessageCard.svelte';
  import UnsentCard from '../lib/UnsentCard.svelte';
  import { buildFeed, dayLabel } from '../lib/format';
  import { page } from '../state.svelte';

  const rows = $derived(page.feed ? buildFeed(page.feed.loaded) : []);
  const pending = $derived(page.feed?.pending ?? []);
  const empty = $derived(rows.length === 0 && pending.length === 0);

  // `scrollTick` is bumped when the feed should follow its newest row: on open,
  // when a page arrived while the reader was at the bottom, and on "newest".
  $effect(() => {
    const tick = page.feed?.scrollTick ?? 0;
    if (tick === 0) return;
    const view = document.getElementById('view');
    if (view) view.scrollTop = view.scrollHeight;
  });
</script>

<div class="vb-screen vb-feed">
  {#if empty}
    <div class="empty">{page.tr('no_messages')}</div>
  {/if}

  {#each rows as row (row.key)}
    {#if row.kind === 'day'}
      <div class="day">{page.tr(dayLabel(row.day, page.langCode))}</div>
    {:else}
      <MessageCard message={row.message} />
    {/if}
  {/each}

  {#each pending as take (take.localId)}
    <UnsentCard {take} />
  {/each}

  {#if page.feed && !page.feed.atBottom}
    <button
      class="jump-newest"
      type="button"
      aria-label={page.tr('open')}
      onclick={() => page.jumpToNewest()}>↓</button
    >
  {/if}
</div>
