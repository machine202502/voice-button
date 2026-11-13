<script lang="ts">
  // One take in the feed.
  //
  // The stage decides the one word on the time line and the few actions that
  // are worth showing without opening the message. The copy shortcut and the
  // whole menu live on the card, but the row itself is the tap target: editing
  // and deleting are words in a sheet, never a row of mystery icons.

  import type { Message } from '../data/types';
  import { page } from '../state.svelte';
  import {
    describe,
    isEdited,
    isWaiting,
    shortTime,
    stageOf,
    textOf,
  } from '../lib/format';

  interface Props {
    message: Message;
  }

  let { message }: Props = $props();

  const info = $derived(describe(stageOf(message)));
  const text = $derived(textOf(message));
  const waiting = $derived(isWaiting(message));
  const pendingEdit = $derived(message.editPending === true);
  const edited = $derived(isEdited(message) || pendingEdit);
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

  const status = $derived.by(() => {
    if (waiting) return { label: 'engine_busy', tone: 'amber' };
    if (info.tone === 'warning') return { label: 'failed', tone: 'red' };
    if (info.stage === 'done') return { label: 'done', tone: 'done' };
    // A finished stage keeps its own word even without a connection: the
    // computer already answered, so "waiting for connection" would be a lie.
    if (info.stage === 'silent') return { label: 'silent', tone: 'muted' };
    if (info.stage === 'cancelled') return { label: 'cancelled', tone: 'muted' };
    if (!page.reachable) return { label: 'waiting_link', tone: 'amber' };
    return { label: 'in_progress', tone: 'amber' };
  });

  // Inline too: no retry when the computer has no audio left to re-read.
  const inlineRetry = $derived(
    !page.archived && info.actions.includes('retry') && message.audio !== false,
  );
  const inlineDelete = $derived(inlineRetry);

  function open(): void {
    page.openMessageSheet(message);
  }

  function keydown(event: KeyboardEvent): void {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    open();
  }
</script>

<div
  class="message"
  class:live={info.tone === 'live'}
  class:failed={info.tone === 'warning'}
  class:archived={page.archived}
  data-id={String(message.id)}
  role="button"
  tabindex="0"
  onclick={open}
  onkeydown={keydown}
>
  <div class="time">
    <span class="stamp">{shortTime(message.recorded)}</span>
    <span class="chip status" class:amber={status.tone === 'amber'} class:red={status.tone === 'red'}>
      {page.tr(status.label)}
    </span>
    {#if edited}
      <span class="chip" class:edited-waiting={pendingEdit} class:edited={!pendingEdit}>
        {page.tr(pendingEdit ? 'edited_waiting' : 'edited')}
      </span>
    {/if}
    <button
      class="copy"
      type="button"
      onclick={(event) => {
        event.stopPropagation();
        void page.copyText(text);
      }}>{page.tr('copy_inline')}</button
    >
  </div>

  <div class="text" class:empty={!text}>{text ? shown : hint}</div>

  {#if long && !inRecording}
    <button
      class="more"
      type="button"
      onclick={(event) => {
        event.stopPropagation();
        expanded = !expanded;
      }}
    >
      {page.tr(expanded ? 'collapse' : 'expand')}
    </button>
  {/if}

  {#if inlineRetry}
    <div class="actions">
      <button
        type="button"
        onclick={(event) => {
          event.stopPropagation();
          void page.retryMessage(message);
        }}>{page.tr('retry')}</button
      >
      {#if inlineDelete}
        <button
          class="danger"
          type="button"
          onclick={(event) => {
            event.stopPropagation();
            page.confirmDeleteMessage(message);
          }}>{page.tr('delete')}</button
        >
      {/if}
    </div>
  {/if}
</div>
