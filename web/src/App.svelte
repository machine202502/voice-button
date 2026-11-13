<script lang="ts">
  // The page: the pairing screen until the phone is paired, then the one shell
  // every screen lives in. There are no tabs; the open monolog, the archive,
  // settings and the editor are the page's own state, so recording and queued
  // work survive navigation.

  import { onMount } from 'svelte';
  import Shell from './lib/Shell.svelte';
  import SheetHost from './lib/SheetHost.svelte';
  import EditScreen from './screens/EditScreen.svelte';
  import FeedScreen from './screens/FeedScreen.svelte';
  import MonologList from './screens/MonologList.svelte';
  import PairScreen from './screens/PairScreen.svelte';
  import SettingsScreen from './screens/SettingsScreen.svelte';
  import { page } from './state.svelte';

  const title = $derived(
    page.screen === 'feed' && page.monolog
      ? page.monolog.name
      : page.screen === 'settings'
        ? page.tr('settings')
        : page.archived
          ? page.tr('archive')
          : page.tr('monologs'),
  );

  onMount(() => {
    void page.boot();
  });

  // The clock of a running take. The interval exists only while it records.
  $effect(() => {
    if (!page.recording) return;
    const timer = window.setInterval(() => page.tickElapsed(), 500);
    return () => window.clearInterval(timer);
  });
</script>

{#if page.mode === 'pair'}
  <PairScreen />
{:else if page.mode === 'app'}
  {#if page.screen === 'edit' && page.editing}
    <EditScreen />
  {:else}
    <Shell {title}>
      {#if page.screen === 'settings'}
        <SettingsScreen />
      {:else if page.screen === 'feed' && page.feed}
        <FeedScreen />
      {:else}
        <MonologList />
      {/if}
    </Shell>
    <SheetHost />
  {/if}
{/if}
