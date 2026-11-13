<script lang="ts">
  // The full-screen text editor. It is a screen, not a sheet: the keyboard takes
  // half the phone, and two lines of text between sheet buttons was the wrong
  // place to edit a paragraph. Saving is local first, so the button never waits
  // for the computer.

  import { untrack } from 'svelte';
  import { textOf } from '../lib/format';
  import { page } from '../state.svelte';

  const original = $derived(page.editing ? textOf(page.editing) : '');
  let draft = $state(untrack(() => (page.editing ? textOf(page.editing) : '')));
  let saving = $state(false);

  const changed = $derived(draft !== original);
  const disabled = $derived(!changed || saving);

  let box = $state<HTMLTextAreaElement | null>(null);

  $effect(() => {
    box?.focus();
  });

  async function save(): Promise<void> {
    if (disabled) return;
    saving = true;
    try {
      await page.saveEdit(draft);
    } finally {
      saving = false;
    }
  }
</script>

{#if page.editing}
  <div class="vb-edit">
    <header>
      <button class="vb-edit-cancel" id="edit-cancel" type="button" onclick={() => page.closeEdit()}>
        {page.tr('cancel')}
      </button>
      <h1 id="title">{page.tr('edit_short')}</h1>
      <button
        class="vb-edit-save"
        id="edit-save"
        type="button"
        {disabled}
        onclick={() => void save()}>{page.tr('save')}</button
      >
    </header>

    <main id="view" class="vb-edit-main">
      <textarea
        class="vb-edit-field"
        bind:this={box}
        bind:value={draft}
        aria-label={page.tr('edit_short')}
        spellcheck="true"
      ></textarea>
      <p class="vb-edit-note">{page.tr('edit_note')}</p>
    </main>
  </div>
{/if}
