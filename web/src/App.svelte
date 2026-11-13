<script lang="ts">
  // The page: the pairing screen until the phone is paired, then the shell with
  // its three tabs, the composer and whatever sheet is open.
  //
  // The tabs are the shell's own; the open monolog, the archive and the sheets
  // are state, so switching tabs never loses a take and the recording keeps
  // running wherever the person goes.

  import { Tabs } from '@skeletonlabs/skeleton-svelte';
  import { onMount } from 'svelte';
  import Composer from './lib/Composer.svelte';
  import Shell from './lib/Shell.svelte';
  import SheetHost from './lib/SheetHost.svelte';
  import MonologsScreen from './screens/MonologsScreen.svelte';
  import PairScreen from './screens/PairScreen.svelte';
  import RecordScreen from './screens/RecordScreen.svelte';
  import SettingsScreen from './screens/SettingsScreen.svelte';
  import { page } from './state.svelte';

  type TabName = 'monologs' | 'record' | 'settings';

  let tab = $state<TabName>('monologs');

  const title = $derived(
    page.screen === 'feed' && page.monolog
      ? page.monolog.name
      : page.tr(page.archived ? 'archive' : 'monologs'),
  );

  function selectTab(value: string): void {
    if (value === 'monologs' || value === 'record' || value === 'settings') tab = value;
  }

  /** Back leaves the monolog (or the archive) and lands on the list tab. */
  function back(): void {
    tab = 'monologs';
    page.back();
  }

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
  <Tabs value={tab} onValueChange={(details) => selectTab(details.value)} class="contents">
    <Shell {title} onback={back} composerVisible={tab === 'monologs'}>
      {#snippet tabs()}
        <div class="vb-tabs-wrap">
          <Tabs.List class="vb-tabs" aria-label={page.tr('monologs')}>
            <Tabs.Trigger value="monologs" class="vb-tab">{page.tr('monologs')}</Tabs.Trigger>
            <Tabs.Trigger value="record" class="vb-tab">{page.tr('record')}</Tabs.Trigger>
            <Tabs.Trigger value="settings" class="vb-tab" title="⚙">⚙</Tabs.Trigger>
          </Tabs.List>
        </div>
      {/snippet}

      {#snippet children()}
        <Tabs.Content value="monologs">
          <MonologsScreen />
        </Tabs.Content>
        <Tabs.Content value="record">
          <RecordScreen />
        </Tabs.Content>
        <Tabs.Content value="settings">
          <SettingsScreen />
        </Tabs.Content>
      {/snippet}

      {#snippet composer()}
        <Composer />
      {/snippet}
    </Shell>
  </Tabs>
  <SheetHost />
{/if}
