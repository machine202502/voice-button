<script lang="ts">
  // One monolog in the list, and in the archive.
  //
  // The card carries only what can be known without opening it: the name, the
  // time and message count, the last words, and the one status that matters
  // (transcribing or waiting for the computer). Opening it is a tap. A
  // horizontal pull in either direction archives it — or takes it back out of
  // the archive — and flies the card out; vertical movement is left to the
  // scrolling list.

  import type { Monolog } from '../data/types';
  import { page } from '../state.svelte';
  import { dayKey, dayLabel, pluralKey, shortTime } from '../lib/format';

  interface Props {
    monolog: Monolog;
  }

  let { monolog }: Props = $props();

  const id = $derived(Number(monolog.id));
  const waiting = $derived(
    page.list.pending.filter((take) => Number(take.monolog) === id && !take.remote).length,
  );
  const recordingHere = $derived(page.recording !== null && page.recording.monologId === id);
  const working = $derived(monolog.busy === true || recordingHere);
  const unsent = $derived(waiting > 0);
  const count = $derived(monolog.local === true ? waiting : (monolog.count ?? 0) + waiting);
  const selected = $derived(page.isSelected(id));

  const stamp = $derived(monolog.lastAt || monolog.created || '');
  const day = $derived(stamp ? dayLabel(dayKey(stamp), page.langCode) : '');
  const clock = $derived(shortTime(stamp));
  const meta = $derived(
    [day ? page.tr(day) : '', clock, count + ' ' + page.tr(pluralKey(count, page.langCode))]
      .filter(Boolean)
      .join(' · '),
  );
  const preview = $derived(monolog.last || page.tr('no_preview'));
  const status = $derived(
    working
      ? { label: 'card_transcribing', tone: 'lime' }
      : unsent
        ? { label: 'waiting_link', tone: 'amber' }
        : null,
  );

  let card = $state<HTMLDivElement | null>(null);
  let dx = $state(0);
  let dragging = $state(false);
  let leaving = $state(false);
  let startX = 0;
  let startY = 0;
  let dy = 0;
  let axis: 'x' | 'y' | null = null;
  let active = false;
  let suppress = false;

  const gestureEnabled = $derived(!page.selection && !recordingHere && !leaving);

  function reset(): void {
    dx = 0;
    dragging = false;
    leaving = false;
    axis = null;
    active = false;
  }

  function down(event: PointerEvent): void {
    if (!gestureEnabled) return;
    if (event.target instanceof Element && event.target.closest('button')) return;
    if (event.pointerType === 'mouse' && event.button !== 0) return;
    active = true;
    axis = null;
    dx = 0;
    dy = 0;
    suppress = false;
    startX = event.clientX;
    startY = event.clientY;
    dragging = true;
    try {
      card?.setPointerCapture(event.pointerId);
    } catch {
      // Synthetic pointers cannot be captured; the swipe still works.
    }
  }

  function move(event: PointerEvent): void {
    if (!active) return;
    const x = event.clientX - startX;
    dy = event.clientY - startY;
    if (!axis) {
      if (Math.abs(x) < 10 && Math.abs(dy) < 10) return;
      axis = Math.abs(x) > Math.abs(dy) ? 'x' : 'y';
      if (axis === 'y') {
        // The list owns vertical movement: let the browser scroll.
        try {
          card?.releasePointerCapture(event.pointerId);
        } catch {
          // A synthetic pointer was never captured.
        }
        active = false;
        dragging = false;
        dx = 0;
        axis = null;
        return;
      }
    }
    if (axis !== 'x') return;
    if (event.cancelable) event.preventDefault();
    dx = x;
  }

  function up(): void {
    if (!active) return;
    active = false;
    // A short diagonal move is still a tap; only a clear sideways pull counts.
    if (Math.abs(dx) < 72 || Math.abs(dx) <= Math.abs(dy)) {
      reset();
      return;
    }
    const target = !page.archived;
    suppress = true;
    dragging = false;
    leaving = true;
    dx = (dx < 0 ? -1 : 1) * 520;
    window.setTimeout(() => {
      void page.setArchived(monolog, target).catch(() => {
        suppress = false;
        reset();
      });
    }, 190);
  }

  function cancelGesture(): void {
    if (!active) return;
    reset();
  }

  function click(event: MouseEvent): void {
    if (suppress) {
      suppress = false;
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    if (page.selection) {
      page.toggleSelected(id);
      return;
    }
    void page.openMonolog(monolog);
  }

  function keydown(event: KeyboardEvent): void {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    if (page.selection) page.toggleSelected(id);
    else void page.openMonolog(monolog);
  }
</script>

<div class="swipe-row" class:dragging class:leaving>
  <div class="swipe-action" class:out={page.archived}>
    {page.tr(page.archived ? 'restore' : 'archive')}
  </div>
  <div
    class="monolog-card"
    class:active={working}
    class:unsent={unsent}
    class:selected
    role="button"
    tabindex="0"
    data-id={String(id)}
    data-swipe={page.archived ? 'unarchive' : 'archive'}
    data-selected={selected ? 'true' : undefined}
    aria-pressed={page.selection ? selected : undefined}
    title={working ? page.tr('card_working') : unsent ? page.tr('card_unsent') : ''}
    style:transform={dx === 0 ? null : `translate3d(${dx}px,0,0)`}
    bind:this={card}
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={cancelGesture}
    onclick={click}
    onkeydown={keydown}
  >
    <div class="name-row">
      {#if status}
        <span class="dot" class:amber={status.tone === 'amber'}></span>
      {/if}
      <strong>{monolog.name}</strong>
      {#if page.selection}
        <span class="check" aria-hidden="true">{selected ? '✓' : ''}</span>
      {/if}
    </div>
    <div class="meta">{meta}</div>
    <p class="preview">{preview}</p>
    {#if status}
      <span class="chip" class:lime={status.tone === 'lime'} class:amber={status.tone === 'amber'}>
        {page.tr(status.label)}
      </span>
    {/if}
    {#if page.archived && !page.selection}
      <button
        class="restore"
        type="button"
        onclick={(event) => {
          event.stopPropagation();
          void page.setArchived(monolog, false);
        }}>{page.tr('restore')}</button
      >
    {/if}
  </div>
</div>
