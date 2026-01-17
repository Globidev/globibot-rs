<script lang="ts">
  import DiscordLogin from '../../../lib/components/DiscordLogin.svelte';
  import { authStore } from '../../../lib/stores/auth.svelte';
</script>

<div class="mx-auto max-w-5xl px-6">
  <!-- Header -->
  <header class="mb-12 text-center">
    <h1 class="text-2xl font-semibold tracking-tight">LLM plugin</h1>
  </header>

  <div class="flex flex-col items-center gap-6 text-center">
    {#if authStore.user == null}
      <div class="text-sm">You must be logged in to access the LLM plugin.</div>
      <DiscordLogin />
    {:else}
      <div class="text-9xl">🚧</div>
    {/if}
  </div>
</div>
