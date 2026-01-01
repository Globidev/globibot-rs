<script lang="ts">
  import type { Plugin } from '$lib/types';

  interface Props {
    plugin: Plugin;
  }

  let { plugin }: Props = $props();
</script>

<a
  class="rounded-xl border border-gray-200 bg-white p-5 shadow-sm transition hover:shadow-md dark:border-gray-700 dark:bg-gray-900"
  href={`/plugins/${plugin.name}`}
>
  <!-- Plugin name -->
  <h2 class="mb-4 text-lg font-medium">
    {plugin.name}
  </h2>

  <!-- Capabilities -->
  <div class="flex items-center gap-4 text-sm">
    <div class={['flex items-center gap-1', { invisible: !plugin.has_events }]}>
      <span class="icon-[mdi--broadcast] text-green-500"></span>
      <span>Events</span>
    </div>

    <div class={['flex items-center gap-1', { invisible: !plugin.has_rpc }]}>
      <span class="icon-[mdi--remote] text-blue-500"></span>
      <span>RPC</span>
    </div>
  </div>
</a>
