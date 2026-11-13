<script lang="ts">
  // A note that can be dismissed two ways: a tap (with the cross as the visible
  // affordance) or a swipe to either side. The swipe follows the finger and, past
  // the threshold, the banner leaves the way the finger went; below it, it springs
  // back. A swipe never turns into a tap.

  let { text, id, onDismiss }: { text: string; id: number; onDismiss: (id: number) => void } =
    $props();

  const THRESHOLD = 64;

  let offset = $state(0);
  let leaving = $state(false);
  let dragging = $state(false);

  let startX = 0;
  let startY = 0;
  let axis: 'none' | 'x' | 'y' = 'none';
  let moved = false;

  function down(event: PointerEvent): void {
    if (event.pointerType === 'mouse' && event.button !== 0) return;
    dragging = true;
    moved = false;
    axis = 'none';
    startX = event.clientX;
    startY = event.clientY;
    (event.currentTarget as HTMLElement).setPointerCapture(event.pointerId);
  }

  function move(event: PointerEvent): void {
    if (!dragging) return;
    const dx = event.clientX - startX;
    const dy = event.clientY - startY;
    if (axis === 'none') {
      if (Math.abs(dx) < 8 && Math.abs(dy) < 8) return;
      axis = Math.abs(dx) >= Math.abs(dy) ? 'x' : 'y';
      if (axis === 'y') {
        // A vertical gesture belongs to the list, not to the note.
        endDrag(event);
        return;
      }
    }
    if (axis !== 'x') return;
    moved = true;
    offset = dx;
  }

  function up(event: PointerEvent): void {
    if (!dragging) return;
    const travelled = offset;
    const swiped = Math.abs(travelled) >= THRESHOLD;
    endDrag(event);
    if (swiped) {
      leaving = true;
      onDismiss(id);
      return;
    }
    offset = 0;
  }

  function endDrag(event: PointerEvent): void {
    dragging = false;
    try {
      (event.currentTarget as HTMLElement).releasePointerCapture(event.pointerId);
    } catch {
      // The pointer may already be gone.
    }
  }

  function click(): void {
    // A drag must not also count as a tap.
    if (moved) {
      moved = false;
      return;
    }
    onDismiss(id);
  }
</script>

<button
  class="banner"
  class:leaving
  class:dragging
  type="button"
  style:transform="translateX({offset}px)"
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={endDrag}
  onclick={click}
>
  <span class="banner-text">{text}</span>
  <span class="banner-close" aria-hidden="true">×</span>
</button>
