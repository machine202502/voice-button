<script lang="ts">
  // The second tab: the same microphone as the composer, with room around it.
  // It records into the monolog that is open; with none open it is disabled and
  // says so instead of pretending.

  import MicIcon from '../lib/MicIcon.svelte';
  import RecordRings from '../lib/RecordRings.svelte';
  import { formatDuration } from '../lib/time';
  import { page } from '../state.svelte';

  const recording = $derived(page.recording !== null);
  const working = $derived(recording && page.reachable);
  const offline = $derived(recording && !page.reachable);
  const disabled = $derived(page.monolog === null || page.archived);

  const hint = $derived(
    recording
      ? page.reachable
        ? page.tr('stop_hint')
        : page.tr('record_offline')
      : page.tr('dictate_hint'),
  );
</script>

<div class="vb-screen">
  <div class="vb-record-stage">
    <div class="vb-record-wrap">
      <RecordRings active={working} level={page.level} size={96} />
      <button
        class="vb-record-big"
        class:recording={working}
        class:offline
        type="button"
        aria-label={page.tr('record')}
        aria-pressed={recording}
        {disabled}
        onclick={() => void page.toggleRecording()}
      >
        <MicIcon size={36} />
      </button>
    </div>
    <span class="vb-timer">{recording ? formatDuration(page.elapsed) : ''}</span>
    <p class="vb-record-hint">{hint}</p>
    {#if page.monolog}
      <p class="vb-record-hint vb-record-name">{page.monolog.name}</p>
    {/if}
  </div>
</div>
