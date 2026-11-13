<script lang="ts">
  // One take in the feed.
  //
  // The stage is the only input that decides what the card says and which
  // buttons it offers: a take still being transcribed shows its stage and can
  // only be deleted, a failed one offers a retry, a done one can be copied,
  // edited or deleted. In the archive the card is read-only.

  import type { Message } from '../data/types';
  import { page } from '../state.svelte';
  import { describe, isEdited, isWaiting, shortTime, stageOf, textOf } from '../lib/format';

  interface Props {
    message: Message;
  }

  let { message }: Props = $props();

  const info = $derived(describe(stageOf(message)));
  const text = $derived(textOf(message));
  const waiting = $derived(isWaiting(message));
  const long = $derived(text.length > 320);
  let expanded = $state(false);

  // While the microphone is open, the card it is writing into is always fully
  // shown: the person must see every word of the take in progress.
  const inRecording = $derived(
    page.recording !== null && Number(page.recording.take.remote) === Number(message.id),
  );
  const shown = $derived(
    long && !expanded && !inRecording ? text.slice(0, 240).trimEnd() + '…' : text,
  );
  const hint = $derived(
    waiting ? page.tr('busy_hint') : info.hint ? page.tr(info.hint) : '',
  );
  // Read-only archive: changing or deleting starts by pulling the monolog out
  // of the archive.
  const actions = $derived(
    info.actions.filter((action) => !(page.archived && action !== 'copy')),
  );
</script>

<article
  class="message"
  class:live={info.tone === 'live'}
  class:failed={info.tone === 'warning'}
  data-id={String(message.id)}
>
  <div class="time">
    <span>{shortTime(message.recorded)}</span>
    {#if info.stage !== 'done'}
      <span class="chip" class:live={info.tone === 'live'} class:failed={info.tone === 'warning'}>
        {page.tr(info.label)}
      </span>
    {/if}
    {#if waiting}
      <span class="chip busy">{page.tr('engine_busy')}</span>
    {/if}
    {#if isEdited(message)}
      <span class="chip edited">{page.tr('edited')}</span>
    {/if}
  </div>
  <div class="text" class:empty={!text}>{text ? shown : hint}</div>
  {#if long && !inRecording}
    <button class="more" type="button" onclick={() => (expanded = !expanded)}>
      {page.tr(expanded ? 'collapse' : 'expand')}
    </button>
  {/if}
  {#if actions.length > 0}
    <div class="actions">
      {#each actions as action (action)}
        {#if action === 'copy'}
          <button class="action" type="button" onclick={() => void page.copyText(text)}>
            {page.tr('copy')}
          </button>
        {:else if action === 'edit'}
          <button class="action" type="button" onclick={() => page.openEdit(message)}>
            {page.tr('edit')}
          </button>
        {:else if action === 'retry'}
          <button class="action" type="button" onclick={() => void page.retryMessage(message)}>
            {page.tr('retry')}
          </button>
        {:else}
          <button class="action danger" type="button" onclick={() => page.openDelete(message)}>
            {page.tr('delete')}
          </button>
        {/if}
      {/each}
    </div>
  {/if}
</article>
