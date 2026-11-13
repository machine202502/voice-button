<script lang="ts">
  // An overlay scroll indicator for the one scrolling container.
  //
  // The native scrollbar is hidden: on a phone it takes width away from the
  // content, and its look is the browser's, not ours. This bar floats above the
  // content, never changes the layout, fades away when nothing moves and can be
  // dragged like the real thing.

  let { target }: { target: HTMLElement | null } = $props();

  const MIN_THUMB = 36;
  const FADE_AFTER_MS = 900;

  let trackHeight = $state(0);
  let thumbTop = $state(0);
  let thumbHeight = $state(0);
  let visible = $state(false);
  let dragging = $state(false);

  let hideTimer: number | undefined;

  function measure(): void {
    if (!target) return;
    const { scrollTop, scrollHeight, clientHeight } = target;
    trackHeight = clientHeight;
    const ratio = scrollHeight > 0 ? clientHeight / scrollHeight : 1;
    thumbHeight = Math.max(MIN_THUMB, Math.round(clientHeight * ratio));
    const travel = clientHeight - thumbHeight;
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
    visible = true;
    scheduleHide();
  }

  $effect(() => {
    const element = target;
    if (!element) return;
    measure();
    const onScroll = (): void => {
      measure();
      wake();
    };
    const onResize = (): void => {
      measure();
      wake();
    };
    element.addEventListener('scroll', onScroll, { passive: true });
    const observer = new ResizeObserver(onResize);
    observer.observe(element);
    if (element.firstElementChild) observer.observe(element.firstElementChild);
    return () => {
      window.clearTimeout(hideTimer);
      element.removeEventListener('scroll', onScroll);
      observer.disconnect();
    };
  });

  function jumpTo(event: PointerEvent): void {
    if (!target) return;
    const box = (event.currentTarget as HTMLElement).getBoundingClientRect();
    const offset = event.clientY - box.top - thumbHeight / 2;
    const travel = box.height - thumbHeight;
    if (travel <= 0) return;
    const ratio = Math.min(1, Math.max(0, offset / travel));
    target.scrollTop = ratio * (target.scrollHeight - target.clientHeight);
  }

  function startDrag(event: PointerEvent): void {
    if (!target) return;
    event.preventDefault();
    event.stopPropagation();
    dragging = true;
    visible = true;
    const startY = event.clientY;
    const startTop = target.scrollTop;
    const travel = trackHeight - thumbHeight;
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

{#if visible && trackHeight > 0}
  <div
    class="vb-scroll-track"
    role="presentation"
    onpointerdown={jumpTo}
    style:height="{trackHeight}px"
  >
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
