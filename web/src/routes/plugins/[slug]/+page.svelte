<script lang="ts">
  import { pluginStore } from '$lib/stores/plugins';
  import type { PageProps } from './$types';

  let { params }: PageProps = $props();

  const plugin = $derived(pluginStore.pluginBySlug(params.slug));
</script>

{#if !$plugin}
  <div class="mx-auto max-w-5xl px-6 py-16 text-center text-sm">
    Plugin '{params.slug}' not found.
  </div>
{:else}
  <div class="mx-auto max-w-5xl px-6">
    <!-- Header -->
    <header class="mb-12 text-center">
      <h1 class="text-2xl font-semibold tracking-tight">{$plugin.name} plugin</h1>
    </header>

    <div class="flex flex-col items-center gap-6 text-center">
      <span>This plugin does not expose a Web UI</span>
    </div>

    <!-- <iframe
      class="border border-amber-50"
      width="100%"
      height="600"
      title="Plugin UI"
      src={`/plugin-api/${$plugin.name}/`}
    ></iframe> -->
  </div>
{/if}
