<script lang="ts">
  // The third tab: the phone's side of the setup.
  //
  // Theme, language, the computer this phone is paired with, the page's own
  // version, the install guide and a reload. There is no unpair button because
  // the old page had none either: a phone is paired by the computer's QR code,
  // and the way out of a wrong pairing is a new one on the computer side.
  //
  // The theme switch is a Skeleton Switch and the pairing dot is our own; the
  // settings have no catalog keys beyond the ones added for the theme, so the
  // screen leans on `theme`/`dark`/`light`, `site_version`, `install_app`,
  // `reload` and `pair_required`. The language names are already in the
  // language they name.

  import { Switch } from '@skeletonlabs/skeleton-svelte';
  import { onMount } from 'svelte';
  import { languages } from '../i18n';
  import { page } from '../state.svelte';

  const entries = languages();
  const address = location.origin;

  const currentName = $derived(
    entries.find((entry) => entry.code === page.langCode)?.name ?? page.langCode,
  );
  const stateLabel = $derived(
    page.paired === true ? 'API ' + String(page.apiVersion) : page.tr('pair_required'),
  );

  onMount(() => {
    void page.refreshMe();
  });
</script>

<div class="vb-screen">
  <section class="vb-panel">
    <Switch
      checked={page.theme === 'light'}
      onCheckedChange={(details) => page.setTheme(details.checked ? 'light' : 'dark')}
      class="vb-switch-row"
    >
      <span class="vb-switch-copy">
        <Switch.Label>{page.tr('theme')}</Switch.Label>
        <span class="vb-switch-desc">{page.tr('dark')} / {page.tr('light')}</span>
      </span>
      <span class="vb-setting-state">{page.tr(page.theme === 'light' ? 'light' : 'dark')}</span>
      <Switch.Control class="vb-switch-control">
        <Switch.Thumb class="vb-switch-thumb" />
      </Switch.Control>
      <Switch.HiddenInput />
    </Switch>
  </section>

  <section class="vb-panel">
    <select
      class="vb-select"
      aria-label={currentName}
      value={page.langCode}
      onchange={(event) => void page.changeLanguage(event.currentTarget.value)}
    >
      {#each entries as entry (entry.code)}
        <option value={entry.code}>{entry.name}</option>
      {/each}
    </select>
  </section>

  <section class="vb-panel">
    <div class="vb-setting-row">
      <span
        class="vb-dot"
        class:ok={page.paired === true}
        class:bad={page.paired === false}
        aria-hidden="true"
      ></span>
      <code class="vb-mono">{address}</code>
      <span class="vb-setting-state" class:bad={page.paired === false}>{stateLabel}</span>
    </div>
    {#if page.paired === false}
      <p class="vb-setting-note">{page.tr('pair_required')}</p>
    {/if}
  </section>

  <section class="vb-panel">
    <div class="vb-setting-row">
      <span>{page.tr('site_version')}</span>
      <code class="vb-mono">{page.versionLabel || '—'}</code>
    </div>
    <a class="vb-setting-row" href="/install">
      {page.tr('install_app')}
      <span class="vb-setting-go" aria-hidden="true">→</span>
    </a>
    <button class="vb-setting-row" type="button" onclick={() => page.reload()}>
      {page.tr('reload')}
    </button>
  </section>
</div>
