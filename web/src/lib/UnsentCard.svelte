<script lang="ts">
  // A take that is still only on the phone: recorded, stored, not yet taken by
  // the computer. It says when it was recorded and that it is waiting for a
  // connection; retrying it means asking the sync loop to run now, and deleting
  // it always asks first.

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
    <span class="stamp">{shortTime(take.recorded)}</span>
    <span class="chip unsent">{page.tr('waiting_link')}</span>
  </div>
  <div class="text empty">{page.tr('waiting_connection')}</div>
  <div class="actions">
    <button type="button" onclick={() => page.retryTake()}>{page.tr('retry')}</button>
    <button class="danger" type="button" onclick={() => page.confirmDeleteTake(take)}>
      {page.tr('delete')}
    </button>
  </div>
</article>
