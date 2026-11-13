<script lang="ts">
  // Which sheet is open, and what its buttons mean.
  //
  // The state module only says what the sheet is about (a monolog menu, a
  // message, a confirmation); labels and actions live here, so a language
  // switch or a new flow needs no state change.

  import { describe, stageOf, textOf } from '../lib/format';
  import { page } from '../state.svelte';
  import Sheet from './Sheet.svelte';

  interface Item {
    label: string;
    detail?: string;
    style?: 'primary' | 'danger';
    className?: string;
    action: () => void;
  }

  const sheet = $derived(page.sheet);
  const message = $derived(sheet?.message ?? null);
  const messageText = $derived(message ? textOf(message) : '');
  const messageInfo = $derived(message ? describe(stageOf(message)) : describe(''));

  const items = $derived.by<Item[]>(() => {
    if (!sheet) return [];

    if (sheet.kind === 'list-menu') {
      const rows: Item[] = [
        {
          label: page.tr('new_monolog'),
          className: 'new-monolog',
          action: () => page.openCreate(true),
        },
      ];
      if (!sheet.archivedView) {
        rows.push({
          label: page.tr('archive'),
          detail: String(page.archiveCount),
          action: () => page.openArchiveFromMenu(),
        });
      }
      rows.push(
        { label: page.tr('select_several'), action: () => page.beginSelection() },
        { label: page.tr('settings'), action: () => page.openSettingsFromMenu() },
      );
      return rows;
    }

    if (sheet.kind === 'monolog-menu') {
      const monolog = sheet.monolog;
      if (!monolog) return [];
      return [
        { label: page.tr('rename'), action: () => page.openRenameFromMenu() },
        {
          label: page.tr(sheet.archivedView ? 'restore_from_archive' : 'archive'),
          action: () => void page.archiveFromMenu(),
        },
        { label: page.tr('select_several'), action: () => page.beginSelection() },
        {
          label: page.tr('delete_monolog'),
          style: 'danger',
          action: () => page.confirmDeleteMonolog(monolog),
        },
      ];
    }

    if (sheet.kind === 'message-menu') {
      const row = message;
      if (!row) return [];
      if (sheet.archivedView) {
        return [
          {
            label: page.tr('copy'),
            action: () => {
              void page.copyText(messageText);
              page.dismissSheet();
            },
          },
          {
            label: page.tr('restore_from_archive'),
            action: () => void page.restoreFromMessageSheet(),
          },
        ];
      }
      const rows: Item[] = [
        {
          label: page.tr('copy'),
          action: () => {
            void page.copyText(messageText);
            page.dismissSheet();
          },
        },
      ];
      if (messageInfo.stage === 'done') {
        rows.push({ label: page.tr('edit_text'), action: () => page.openEdit(row) });
      }
      // Retrying only means something for a take the computer could not
      // transcribe: the server refuses it for anything else, and offering it on a
      // finished message is a button that can only answer with an error.
      // A retry re-reads the kept audio: a take whose file the computer no longer
      // has cannot be retried, and a button that can only fail is not offered.
      if (messageInfo.stage === 'failed' && row.audio !== false) {
        rows.push({
          label: page.tr('retry_recognition'),
          action: () => {
            page.dismissSheet();
            void page.retryMessage(row);
          },
        });
      }
      rows.push({
        label: page.tr('delete_message'),
        style: 'danger',
        action: () => page.confirmDeleteMessage(row),
      });
      return rows;
    }

    if (sheet.kind === 'delete-message' || sheet.kind === 'delete-take') {
      return [
        {
          label: page.tr('delete'),
          style: 'danger',
          action: () =>
            void (sheet.kind === 'delete-message'
              ? page.deleteMessageConfirmed()
              : page.deleteTakeConfirmed()),
        },
        { label: page.tr('cancel'), action: () => page.dismissSheet() },
      ];
    }

    if (sheet.kind === 'delete-monolog') {
      return [
        {
          label: page.tr('delete'),
          style: 'danger',
          action: () => void page.deleteMonologConfirmed(),
        },
        { label: page.tr('cancel'), action: () => page.dismissSheet() },
      ];
    }

    return [];
  });

  const field = $derived(
    sheet && (sheet.kind === 'create' || sheet.kind === 'rename') ? sheet.field : null,
  );
  const submitLabel = $derived(
    sheet?.kind === 'create' ? page.tr('create') : sheet?.kind === 'rename' ? page.tr('save') : '',
  );
  const oneLine = $derived(sheet?.kind === 'create' || sheet?.kind === 'rename');

  function submit(text: string): void {
    if (sheet?.kind === 'create') void page.createFromSheet(text);
    else if (sheet?.kind === 'rename') void page.renameFromSheet(text);
  }
</script>

{#if sheet}
  {#key sheet}
    <Sheet
      title={sheet.title}
      hint={sheet.hint}
      {field}
      placeholder={sheet.placeholder}
      {oneLine}
      cancel={page.tr('cancel')}
      {items}
      {submitLabel}
      onsubmit={submit}
      ondismiss={() => page.dismissSheet()}
    />
  {/key}
{/if}
