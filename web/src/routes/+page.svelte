<script lang="ts">
  import PluginCard from '../lib/components/PluginCard.svelte';
  import { pluginStore } from '../lib/stores/plugins.js';

  const { plugins } = pluginStore;
</script>

<div class="mx-auto max-w-5xl px-6 py-16">
  <!-- Header -->
  <header class="mb-12 text-center">
    <h1 class="text-2xl font-semibold tracking-tight">Active plugins</h1>
  </header>

  <!-- Empty state -->
  {#if $plugins.length === 0}
    <div class="text-center text-sm">No plugins running yet.</div>
  {:else}
    <!-- Plugin grid -->
    <div class="grid gap-6 sm:grid-cols-2 lg:grid-cols-3">
      {#each $plugins as plugin}
        <PluginCard {plugin} />
      {/each}
    </div>
  {/if}
</div>

<style>
</style>
