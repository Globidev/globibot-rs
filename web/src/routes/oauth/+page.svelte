<script lang="ts">
  import { goto } from '$app/navigation';
  import { onMount } from 'svelte';
  import { authStore, takeReturnPath } from '../../lib/stores/auth.svelte';

  // Set when Discord refused to skip its authorization screen, to retry once with it
  const CONSENT_RETRY_KEY = 'login_consent_retry';

  onMount(async () => {
    const params = new URLSearchParams(window.location.search);
    const code = params.get('code');
    const state = params.get('state');
    const error = params.get('error');

    // Remove the ?code=... from the URL and history
    goto('/oauth', { replaceState: true, keepFocus: true, noScroll: true });

    if (code != null && state != null) {
      sessionStorage.removeItem(CONSENT_RETRY_KEY);
      try {
        await authStore.login(code, state);
        goto(takeReturnPath(), { replaceState: true });
      } catch {
        // The error is shown below
      }
    } else if (error === 'access_denied') {
      // Cancelled on Discord's screen
      goto(takeReturnPath(), { replaceState: true });
    } else if (error != null && sessionStorage.getItem(CONSENT_RETRY_KEY) == null) {
      // `prompt=none` can fail for users who never authorized the app: show the screen this time
      sessionStorage.setItem(CONSENT_RETRY_KEY, '1');
      window.location.href = '/api/discord/authorize?prompt=consent';
    }
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
      <a href="/" class="mt-4 text-blue-500 underline">Go to Home</a>
    </div>
  {:else if authStore.logginError != null}
    <span> Error during Logging flow: {authStore.logginError}</span>
  {:else}
    <span>Not logged in.</span>
  {/if}
</div>
