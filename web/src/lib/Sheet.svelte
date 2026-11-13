<script lang="ts">
  // One bottom sheet: a title, an optional line of explanation, one text field
  // and its buttons. What the buttons do belongs to the caller (`SheetHost`);
  // this file only owns the look and the focus.
  //
  // The field is a textarea even for a monolog name, as on the old page: Chrome
  // offers the address book, cards and passwords over a text input no matter
  // what `autocomplete` says, and a textarea never does.

  import { untrack } from 'svelte';

  interface Button {
    label: string;
    style?: 'primary' | 'danger';
    action: (text: string) => void;
  }

  interface Props {
    title: string;
    hint: string;
    /** The text of the one field, or `null` for a sheet without one. */
    field: string | null;
    placeholder: string;
    /** A monolog name is one line high; an edit is a paragraph. */
    oneLine: boolean;
    cancel: string;
    primary: Button | null;
    secondary: Button | null;
    ondismiss: () => void;
  }

  let {
    title,
    hint,
    field,
    placeholder,
    oneLine,
    cancel,
    primary,
    secondary,
    ondismiss,
  }: Props = $props();

  // The draft starts at what the sheet was opened with and is the person's
  // from then on; a new sheet is a new component, so this is read once.
  let draft = $state(untrack(() => field ?? ''));
  let box = $state<HTMLTextAreaElement | null>(null);

  $effect(() => {
    const node = box;
    if (!node) return;
    node.focus();
    node.setSelectionRange(node.value.length, node.value.length);
  });

  function backdrop(event: MouseEvent): void {
    if (event.target === event.currentTarget) ondismiss();
  }
</script>

<div
  class="sheet-backdrop"
  role="presentation"
  onclick={backdrop}
  onkeydown={(event) => {
    if (event.key === 'Escape') ondismiss();
  }}
>
  <div class="sheet" role="dialog" aria-modal="true" aria-labelledby="sheet-title">
    <h2 id="sheet-title">{title}</h2>
    {#if hint}
      <div class="hint">{hint}</div>
    {/if}
    {#if field !== null}
      <textarea
        class:name={oneLine}
        bind:this={box}
        bind:value={draft}
        placeholder={placeholder}
        autocomplete="off"
        spellcheck="false"
        aria-label={title}
      ></textarea>
    {/if}
    <div class="buttons">
      <button type="button" onclick={ondismiss}>{cancel}</button>
      {#if primary}
        <button type="button" class={primary.style} onclick={() => primary?.action(draft)}>
          {primary.label}
        </button>
      {/if}
    </div>
    {#if secondary}
      <div class="buttons">
        <button type="button" class={secondary.style} onclick={() => secondary?.action(draft)}>
          {secondary.label}
        </button>
      </div>
    {/if}
  </div>
</div>
