<script lang="ts">
  // A take that is still only on the phone: recorded, stored, not yet taken by
  // the computer. It says when it was recorded and that it is waiting for a
  // connection, and it can be thrown away.

  import type { Take } from '../data/types';
  import { page } from '../state.svelte';
  import { shortTime } from '../lib/format';

  interface Props {
    take: Take;
  }

  let { take }: Props = $props();
</script>

<article class="message unsent" data-id={take.localId}>
  <div class="time">
    <span>{shortTime(take.recorded)}</span>
    <span class="chip unsent">{page.tr('not_sent')}</span>
  </div>
  <div class="text empty">{page.tr('waiting_connection')}</div>
  <div class="actions">
    <button class="action danger" type="button" onclick={() => void page.discardTake(take)}>
      {page.tr('delete')}
    </button>
  </div>
</article>
