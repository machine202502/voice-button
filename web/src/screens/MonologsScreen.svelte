<script lang="ts">
  // The first tab: the monolog list, or the feed of the one that is open.

  import { page } from '../state.svelte';
  import FeedScreen from './FeedScreen.svelte';
  import MonologList from './MonologList.svelte';
</script>

{#if page.screen === 'feed' && page.feed}
  <FeedScreen />
{:else}
  <MonologList />
{/if}
