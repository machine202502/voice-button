<script lang="ts">
  // The page without a pairing token: it says so, names the address it was
  // opened on, and offers a reload. This is what a phone shows when the QR code
  // was never used, and what it falls back to when the token stops being
  // accepted.
  //
  // The version stays in the corner here too: a page that cannot pair still
  // has to be able to say which build it is.

  import { page } from '../state.svelte';

  const address = location.origin;
</script>

<div id="pair-error" class="vb-pair">
  <header class="vb-pair-header">
    <span class="vb-version" id="version">{page.versionLabel}</span>
  </header>
  <main>
    <p class="empty" id="pair-text">{page.pairError || page.tr('pair_required')}</p>
    <p class="empty"><code id="pair-where">{address}</code></p>
    <p class="empty">
      <button class="action" id="reload" type="button" onclick={() => page.reload()}>
        {page.tr('reload')}
      </button>
    </p>
  </main>
</div>
