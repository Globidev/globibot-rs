<script lang="ts">
  import type { Plugin } from '$lib/types';
  import { onMount, onDestroy } from 'svelte';
  import { formatUptime } from '$lib/formatUptime';

  interface Props {
    plugin: Plugin;
  }

  let { plugin }: Props = $props();

  let uptime = $state('');
  let interval: number;

  onMount(() => {
    const update = () => {
      uptime = formatUptime(plugin.startup_ts * 1_000);
    };
    update();
    interval = window.setInterval(update, 1_000);
  });

  onDestroy(() => {
    clearInterval(interval);
  });
</script>

<a
  class="flex flex-col gap-2 rounded-xl border border-gray-200 bg-white p-5 shadow-sm transition hover:shadow-md dark:border-gray-700 dark:bg-gray-900"
  href={`/plugins/${plugin.name}`}
>
  <!-- Plugin name -->
  <h2 class="mb-2 text-lg font-medium">
    {plugin.name}
  </h2>

  <!-- Capabilities -->
  <div class="flex items-center gap-4 text-sm">
    <div class={['flex items-center gap-1', { hidden: !plugin.has_events }]}>
      <span class="icon-[mdi--broadcast] text-green-400"></span>
      <span>Events</span>
    </div>

    <div class={['flex items-center gap-1', { hidden: !plugin.has_rpc }]}>
      <span class="icon-[mdi--remote] text-blue-400"></span>
      <span>RPC</span>
    </div>

    <div class={['flex items-center gap-1', { hidden: !plugin.has_web_api }]}>
      <span class="icon-[mdi--web] text-purple-400"></span>
      <span>Web API</span>
    </div>
  </div>

  <div class="flex items-center gap-1 text-sm">
    <span class="icon-[mdi--clock-outline]"></span>
    <span>{uptime}</span>
  </div>
</a>
