<script lang="ts">
  // The strip that keeps a running take in sight outside its monolog.
  //
  // The composer shows the take only while the monolog is open. Back on the
  // list, in the archive or in settings, this plate is the way to see which
  // monolog is recording, how long it has been running, and to stop it. It is
  // an overlay above the composer area, so showing or hiding it never moves
  // the list; its right edge leaves the scroll indicator's gutter alone.

  import { page } from '../state.svelte';
  import { formatDuration } from './time';

  const recording = $derived(page.recording);

  function stop(event: MouseEvent): void {
    event.stopPropagation();
    void page.stopRecording();
  }
</script>

{#if recording}
  <div
    id="rec-plate"
    class="vb-plate"
    data-monolog={String(recording.monologId)}
    role="button"
    tabindex="0"
    aria-label={page.tr('open')}
    onclick={() => page.openRecording()}
    onkeydown={(event) => {
      if (event.target instanceof Element && event.target.closest('button')) return;
      if (event.key !== 'Enter' && event.key !== ' ') return;
      event.preventDefault();
      page.openRecording();
    }}
  >
    <span class="vb-plate-dot" aria-hidden="true"></span>
    <span class="vb-plate-copy">
      <b class="vb-plate-name">{recording.name}</b>
      <span class="vb-plate-meta"
        >{page.tr('recording')} ·
        <span id="rec-plate-timer">{formatDuration(page.elapsed)}</span></span
      >
    </span>
    <button
      id="rec-plate-stop"
      class="vb-plate-stop"
      type="button"
      aria-label={page.tr('stop')}
      onclick={stop}>{page.tr('stop')}</button
    >
  </div>
{/if}
