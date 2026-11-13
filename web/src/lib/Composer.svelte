<script lang="ts">
  // The round button that records, and the two lines that say what it will do.
  //
  // Its colour is a promise about work actually happening: lime when idle, red
  // while the take is moving, amber while the computer cannot be reached. The
  // take is still recorded in the last case; the colour only stops pretending
  // that it is being transcribed.

  import RecordRings from './RecordRings.svelte';
  import { page } from '../state.svelte';
  import { formatDuration } from './time';
  import MicIcon from './MicIcon.svelte';

  const recording = $derived(page.recording !== null);
  const working = $derived(recording && page.reachable);
  const offline = $derived(recording && !page.reachable);
  const disabled = $derived(page.monolog === null || page.archived);

  const title = $derived(
    recording
      ? page.reachable
        ? page.tr('recording')
        : page.tr('recording_offline')
      : page.tr('start_record'),
  );
  const hint = $derived(
    recording
      ? page.reachable
        ? page.tr('stop_hint')
        : page.tr('record_offline')
      : page.tr('dictate_hint'),
  );
</script>

<div class="vb-record-wrap">
  <RecordRings active={working} level={page.level} size={56} halo={12} rings={3} />
  <button
    class="vb-record-button"
    class:recording={working}
    class:offline
    id="record"
    type="button"
    aria-label={page.tr('record')}
    aria-pressed={recording}
    {disabled}
    onclick={() => void page.toggleRecording()}
  >
    <MicIcon size={24} />
  </button>
</div>
<div class="vb-composer-hint" id="rec-hint">
  <b id="rec-title">{title}</b>
  <span id="rec-sub">{hint}</span>
</div>
<span class="vb-composer-timer" id="timer">{recording ? formatDuration(page.elapsed) : ''}</span>
