<script lang="ts">
  // One monolog in the list.
  //
  // Two states matter from here without opening it: the computer is working on
  // it (lime), or a take of it is still only on the phone (amber). The count
  // adds the waiting takes, because the computer's count cannot include what it
  // has not received yet — an offline monolog used to show "0 messages" right
  // after dictating into it.
  //
  // The card is also the swipe target: pulling it left archives it, pulling it
  // right takes it out of the archive. The gesture is the shortcut; the ⋯ sheet
  // does the same thing with words.

  import type { Monolog } from '../data/types';
  import { page } from '../state.svelte';
  import { cardDate, pluralKey } from '../lib/format';

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

  function reset(): void {
    dx = 0;
    dragging = false;
    leaving = false;
    axis = null;
  }

  function down(event: PointerEvent): void {
    if (leaving) return;
    if (event.target instanceof Element && event.target.closest('.more')) return;
    if (event.pointerType === 'mouse' && event.button !== 0) return;
    if (recordingHere) return;
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
        reset();
        active = false;
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
    suppress = true;
    dragging = false;
    leaving = true;
    dx = (dx < 0 ? -1 : 1) * 420;
    window.setTimeout(() => {
      void page.setArchived(monolog, !page.archived);
    }, 190);
  }

  function click(event: MouseEvent): void {
    if (suppress) {
      suppress = false;
      event.preventDefault();
      event.stopPropagation();
      return;
    }
    void page.openMonolog(monolog);
  }

  function keydown(event: KeyboardEvent): void {
    if (event.key !== 'Enter' && event.key !== ' ') return;
    event.preventDefault();
    void page.openMonolog(monolog);
  }
</script>

<div class="swipe-row" class:dragging class:leaving>
  <div class="swipe-action" class:out={page.archived}>
    {page.tr(page.archived ? 'unarchive' : 'archive')}
  </div>
  <div
    class="monolog-card"
    class:active={working}
    class:unsent={unsent}
    role="button"
    tabindex="0"
    data-id={String(id)}
    title={working ? page.tr('card_working') : unsent ? page.tr('card_unsent') : ''}
    style:transform={dx === 0 ? null : `translate3d(${dx}px,0,0)`}
    bind:this={card}
    onpointerdown={down}
    onpointermove={move}
    onpointerup={up}
    onpointercancel={up}
    onclick={click}
    onkeydown={keydown}
  >
    <div class="top">
      <strong>{monolog.name}</strong>
      {#if monolog.local === true}
        <span class="chip unsent">{page.tr('waiting_link')}</span>
      {/if}
      <span class="when">{cardDate(monolog.lastAt || monolog.created)}</span>
      <button
        class="more"
        type="button"
        aria-label={page.tr('monolog')}
        onclick={(event) => {
          event.stopPropagation();
          page.openCardMenu(monolog);
        }}>⋯</button
      >
    </div>
    <div class="last">{monolog.last ?? ''}</div>
    <div class="count">{count} {page.tr(pluralKey(count, page.langCode))}</div>
  </div>
</div>
