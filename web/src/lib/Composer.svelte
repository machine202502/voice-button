<script lang="ts">
  // The round button that records, and the two lines that say what it will do.
  //
  // Its colour is a promise about the microphone, not about the network: the
  // button and the rings are lime whenever a take is running, online or not.
  // The connection may change the wording below the button; it never stops the
  // rings or recolours the button in the middle of a take.

  import RecordRings from './RecordRings.svelte';
  import { page } from '../state.svelte';
  import { formatDuration } from './time';
  import MicIcon from './MicIcon.svelte';

  const recording = $derived(page.recording !== null);
  const offline = $derived(recording && !page.reachable);
  // On the list and in the archive the same bar is shown while a take runs, so it
  // has to say which monolog is being recorded; inside that monolog the title
  // already does.
  const elsewhere = $derived(
    recording && (page.monolog === null || page.monolog.id !== page.recording?.monologId),
  );
  const disabled = $derived(!recording && (page.monolog === null || page.archived));

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
  <RecordRings active={recording} level={page.level} size={56} halo={12} rings={3} />
  <button
    class="vb-record-button"
    class:recording
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
  <b id="rec-title"
    >{title}{#if elsewhere}<span class="vb-composer-name" id="rec-name"
        ><span class="vb-composer-dot">·</span>{page.recording?.name}</span
      >{/if}</b
  >
  <span id="rec-sub">{hint}</span>
</div>
<span class="vb-composer-timer" id="timer">{recording ? formatDuration(page.elapsed) : ''}</span>
