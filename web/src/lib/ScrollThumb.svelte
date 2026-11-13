<script lang="ts">
  // The scroll indicator for the one scrolling container.
  //
  // It is rendered beside `#view` inside the viewport wrapper, never inside the
  // header or the composer, so its geometry cannot reach either of them. The
  // native scrollbar stays hidden, so showing or hiding the thumb never takes
  // width away from a card.

  let { target }: { target: HTMLElement | null } = $props();

  const MIN_THUMB = 36;
  const FADE_AFTER_MS = 900;

  let thumbTop = $state(0);
  let thumbHeight = $state(0);
  let visible = $state(false);
  let dragging = $state(false);
  let hideTimer: number | undefined;

  function measure(): void {
    if (!target) return;
    const { scrollTop, scrollHeight, clientHeight } = target;
    const ratio = scrollHeight > 0 ? clientHeight / scrollHeight : 1;
    thumbHeight = Math.max(MIN_THUMB, Math.round(clientHeight * ratio));
    const travel = Math.max(0, clientHeight - thumbHeight);
    const progress = scrollHeight > clientHeight ? scrollTop / (scrollHeight - clientHeight) : 0;
    thumbTop = Math.round(travel * Math.min(1, Math.max(0, progress)));
    visible = scrollHeight > clientHeight + 2;
  }

  function scheduleHide(): void {
    if (dragging) return;
    window.clearTimeout(hideTimer);
    hideTimer = window.setTimeout(() => {
      if (!dragging) visible = false;
    }, FADE_AFTER_MS);
  }

  function wake(): void {
    measure();
    if (visible) scheduleHide();
  }

  $effect(() => {
    const element = target;
    if (!element) return;
    window.clearTimeout(hideTimer);
    measure();
    if (visible) scheduleHide();

    const onScroll = (): void => wake();
    const observer = new ResizeObserver(() => wake());
    const watch = (): void => {
      observer.disconnect();
      observer.observe(element);
      if (element.firstElementChild) observer.observe(element.firstElementChild);
      wake();
    };
    const mutations = new MutationObserver(watch);
    element.addEventListener('scroll', onScroll, { passive: true });
    mutations.observe(element, { childList: true });
    watch();

    return () => {
      window.clearTimeout(hideTimer);
      element.removeEventListener('scroll', onScroll);
      observer.disconnect();
      mutations.disconnect();
    };
  });

  function startDrag(event: PointerEvent): void {
    if (!target) return;
    event.preventDefault();
    event.stopPropagation();
    dragging = true;
    visible = true;
    const startY = event.clientY;
    const startTop = target.scrollTop;
    const travel = target.clientHeight - thumbHeight;
    const scrollable = target.scrollHeight - target.clientHeight;
    const handle = event.currentTarget as HTMLElement;
    handle.setPointerCapture(event.pointerId);

    const move = (moveEvent: PointerEvent): void => {
      if (travel <= 0) return;
      const delta = ((moveEvent.clientY - startY) / travel) * scrollable;
      target.scrollTop = startTop + delta;
    };
    const end = (): void => {
      dragging = false;
      handle.removeEventListener('pointermove', move);
      handle.removeEventListener('pointerup', end);
      handle.removeEventListener('pointercancel', end);
      scheduleHide();
    };
    handle.addEventListener('pointermove', move);
    handle.addEventListener('pointerup', end);
    handle.addEventListener('pointercancel', end);
  }
</script>

{#if visible}
  <div class="vb-scroll-track" role="presentation">
    <div
      class="vb-scroll-thumb"
      class:dragging
      aria-hidden="true"
      onpointerdown={startDrag}
      style:top="{thumbTop}px"
      style:height="{thumbHeight}px"
    ></div>
  </div>
{/if}
