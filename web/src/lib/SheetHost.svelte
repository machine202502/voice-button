<script lang="ts">
  // Which sheet is open, and what its buttons mean.
  //
  // The state module only says what the sheet is about (create a monolog, the
  // menu of one, edit a message, delete a message); the labels and the calls
  // live here, so a language switch or a different flow needs no state change.

  import { page } from '../state.svelte';
  import Sheet from './Sheet.svelte';

  interface Button {
    label: string;
    style?: 'primary' | 'danger';
    action: (text: string) => void;
  }

  const sheet = $derived(page.sheet);

  const primary = $derived<Button | null>(
    !sheet
      ? null
      : sheet.kind === 'create'
        ? { label: page.tr('create'), style: 'primary', action: (text) => void page.submitSheet(text) }
        : sheet.kind === 'menu'
          ? sheet.archivedView
            ? { label: page.tr('unarchive'), style: 'primary', action: () => void page.archiveFromSheet() }
            : { label: page.tr('save'), style: 'primary', action: (text) => void page.submitSheet(text) }
          : sheet.kind === 'edit'
            ? { label: page.tr('save'), style: 'primary', action: (text) => void page.submitSheet(text) }
            : { label: page.tr('delete'), style: 'danger', action: () => void page.submitSheet('') },
  );

  // The same button does both jobs on an open monolog: rename, or archive it.
  const secondary = $derived<Button | null>(
    sheet && sheet.kind === 'menu' && !sheet.archivedView
      ? { label: page.tr('archive'), action: () => void page.archiveFromSheet() }
      : null,
  );

  const oneLine = $derived(sheet !== null && (sheet.kind === 'create' || sheet.kind === 'menu'));
</script>

{#if sheet}
  <Sheet
    title={sheet.title}
    hint={sheet.hint}
    field={sheet.field}
    placeholder={sheet.placeholder}
    {oneLine}
    cancel={page.tr('cancel')}
    {primary}
    {secondary}
    ondismiss={() => page.dismissSheet()}
  />
{/if}
