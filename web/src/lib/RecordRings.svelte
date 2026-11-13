<script lang="ts">
  // The flutter around the record button.
  //
  // The button itself is a real DOM button: it stays clickable, focusable and
  // announced. This canvas only paints behind it and never touches the layout —
  // it is absolutely positioned and `pointer-events: none`.
  //
  // The rings are variant D from the design mockup: five irregular rings, each
  // with its own frequency, speed and direction of rotation, a couple of lagging
  // ghosts dragging behind for the smear, and a size that follows the speaker's
  // loudness. In silence the halo stays small and keeps breathing, so the widget
  // never looks frozen.

  let {
    active,
    level,
    size = 56,
    // How far the halo may reach beyond the button, and how many rings fit in it.
    // The composer's button sits close to the screen edge, so it asks for a small
    // halo of three rings; the big button on the Record tab has room for the full
    // five.
    halo = size * 0.6,
    rings = 5,
  }: { active: boolean; level: number; size?: number; halo?: number; rings?: number } = $props();

  /**
   * How far the halo reaches beyond the button at full voice, as a share of the
   * button's own size: 56 px of button gets a 34 px halo, the big 104 px one gets
   * 62. A fixed number either vanished on the big button or spilled across the
   * composer on the small one.
   */

  /** Ring count, spread, amplitude, tempo and smear: the parameters agreed on. */
  const SPREAD = 1;
  const AMPLITUDE = 1;
  const SPEED = 1;
  const SMEAR = 1;
  const IDLE = 0.18;

  const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  let canvas = $state<HTMLCanvasElement | null>(null);
  let frame = 0;
  let shown = 0;

  const SPINS = [0.22, -0.15, 0.31, -0.11, 0.18, -0.24, 0.13];
  const POINTS = 64;

  function draw(context: CanvasRenderingContext2D, time: number, value: number): void {
    const side = size + halo * 2;
    const centre = side / 2;
    const radius = size / 2;
    context.clearRect(0, 0, side, side);

    const open = IDLE + (1 - IDLE) * Math.pow(Math.min(1, Math.max(0, value)), 0.85);
    const reach = halo * SPREAD;
    // The rings share the halo evenly, so the outermost one lands on its edge at
    // full voice and nothing is ever drawn outside the canvas.
    const stepFull = Math.max(1.2, (reach - 2) / Math.max(1, rings));

    const glow = context.createRadialGradient(centre, centre, radius, centre, centre, radius + reach * open);
    glow.addColorStop(0, `rgba(199,243,107,${(0.05 + value * 0.14) * open})`);
    glow.addColorStop(1, 'rgba(199,243,107,0)');
    context.fillStyle = glow;
    context.beginPath();
    context.arc(centre, centre, radius + reach * open, 0, Math.PI * 2);
    context.fill();

    const edge = (ring: number, when: number): number[] => {
      const spin = (SPINS[ring % SPINS.length] ?? 0.2) * SPEED;
      const phase = ring * 1.7;
      const step = 1.2 + (stepFull - 1.2) * open;
      const base = radius + (ring + 1) * step * SPREAD;
      const amplitude = (1.4 + value * 18 * AMPLITUDE) * (1 - ring * 0.1) * SPREAD;
      const points: number[] = [];
      for (let index = 0; index <= POINTS; index += 1) {
        const angle = (index / POINTS) * Math.PI * 2 + spin * when + phase * 0.15;
        const wobble =
          Math.sin(angle * (3 + ring * 0.9) + when * 1.9 * (1 + ring * 0.14) * SPEED + phase) * 0.5 +
          Math.sin(angle * (5 + ring * 1.7) - when * 1.3 * (1 + ring * 0.09) * SPEED + phase * 1.3) * 0.32 +
          Math.sin(angle * (8 + ring * 2.6) + when * 0.8 * (1 + ring * 0.21) * SPEED + phase * 0.7) * 0.18;
        const distance = base + wobble * amplitude;
        points.push(centre + Math.cos(angle) * distance, centre + Math.sin(angle) * distance);
      }
      return points;
    };

    const stroke = (points: number[], width: number, alpha: number): void => {
      if (alpha <= 0.004) return;
      context.beginPath();
      context.moveTo(points[0] ?? 0, points[1] ?? 0);
      for (let index = 2; index < points.length; index += 2) {
        context.lineTo(points[index] ?? 0, points[index + 1] ?? 0);
      }
      context.closePath();
      context.strokeStyle = `rgba(199,243,107,${alpha})`;
      context.lineWidth = width;
      context.lineJoin = 'round';
      context.stroke();
    };

    const trail = 0.055 * SMEAR;
    for (let ring = 0; ring < rings; ring += 1) {
      const alpha = (0.5 - ring * 0.06) * (0.55 + value * 0.45) * (0.62 + 0.38 * open);
      if (SMEAR > 0) {
        for (let ghost = 2; ghost >= 1; ghost -= 1) {
          stroke(edge(ring, time - trail * ghost), 3.2, (alpha * 0.1) / ghost);
        }
      }
      stroke(edge(ring, time), 7.5, alpha * 0.1);
      stroke(edge(ring, time), 3.2, alpha * 0.3);
      stroke(edge(ring, time), 1.15, alpha * 0.85);
    }
  }

  // One effect owns the loop, so it can never be left in the state "started once,
  // cancelled by another effect, never restarted": the loop lives exactly as long
  // as the take does.
  $effect(() => {
    const element = canvas;
    const running = active;
    const side = size + halo * 2;
    const ratio = Math.min(window.devicePixelRatio || 1, 2);
    if (!element) return;
    element.width = side * ratio;
    element.height = side * ratio;
    element.style.width = side + 'px';
    element.style.height = side + 'px';
    const context = element.getContext('2d');
    if (!context) return;
    context.scale(ratio, ratio);

    if (!running) {
      shown = 0;
      context.clearRect(0, 0, side, side);
      return;
    }

    let last = performance.now();
    const tick = (now: number): void => {
      const delta = Math.min((now - last) / 1000, 0.05);
      last = now;
      // The visible size chases the measured loudness: a hard cut on a syllable
      // would look like a glitch, not like breathing.
      const target = Math.min(1, Math.max(0, level));
      const rate = target > shown ? 18 : 5;
      shown += (target - shown) * Math.min(1, delta * rate);
      draw(context, now / 1000, shown);
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  });
</script>

<canvas
  class="vb-record-rings"
  class:live={active}
  bind:this={canvas}
  aria-hidden="true"
  style:--rings-side="{size + halo * 2}px"
  data-reduced={reduced ? 'yes' : 'no'}
></canvas>
