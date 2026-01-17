<script lang="ts">
  import { goto } from '$app/navigation';
  import { onMount } from 'svelte';
  import { authStore } from '../../lib/stores/auth.svelte';

  onMount(() => {
    // Immediately replace the URL with just "/oauth"
    // effectively deleting the ?code=... from history
    goto('/oauth', { replaceState: true, keepFocus: true, noScroll: true });
  });
</script>

<div class="mx-auto max-w-5xl px-6 py-16 text-center text-sm">
  {#if authStore.isLoggingIn}
    <span>Finalizing login...</span>
  {:else if authStore.user != null}
    <div class="flex flex-col items-center gap-4">
      <span>Successfully logged in as</span>
      <span class="font-semibold">{authStore.user.username}</span>
      <img class="h-24 w-24 rounded-lg" src={authStore.user.avatar_url} alt="User Avatar" />
    </div>
  {:else if authStore.logginError != null}
    <span> Error during Logging flow</span>
  {:else}
    <span>Not logged in.</span>
  {/if}
</div>
