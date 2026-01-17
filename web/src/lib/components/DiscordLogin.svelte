<script lang="ts">
  import { clickOutside } from '../actions/clickOutside';
  import { authStore } from '$lib/stores/auth.svelte';

  import discordIcon from '$lib/assets/discord-icon.svg';

  let open = $state(false);
</script>

<div class="relative" use:clickOutside={() => (open = false)}>
  {#if authStore.initialized}
    <button
      class="flex cursor-pointer items-center gap-1 rounded-md"
      onclick={() => (open = !open)}
    >
      {#if authStore.user != null}
        <div class="flex items-center gap-3">
          {#if authStore.user.avatar_url}
            <img src={authStore.user.avatar_url} alt="User Avatar" class="h-8 w-8 rounded-lg" />
          {/if}
          <span>{authStore.user.username}</span>
        </div>
      {:else}
        <a
          href="/api/discord/authorize"
          class="bg-primary hover:bg-primary-hover rounded-md border border-gray-300 px-4 py-2 text-sm hover:bg-gray-100 hover:text-black"
        >
          <div class="flex flex-row items-center gap-2">
            Log in with <img src={discordIcon} alt="Discord Icon" class="inline h-4 w-4" />
          </div>
        </a>
      {/if}
    </button>
  {/if}

  {#if open && authStore.user != null}
    <div
      class="absolute right-0 mt-2 w-48 rounded-lg border border-gray-200 bg-white shadow-lg dark:border-gray-700 dark:bg-gray-900"
    >
      <ul class="py-1 text-sm text-white">
        <li>
          <a
            href={`#`}
            class="block px-4 py-2 hover:bg-gray-100 dark:hover:bg-gray-800"
            onclick={() => {
              open = false;
              authStore.logout();
            }}
          >
            Logout
          </a>
        </li>
      </ul>
    </div>
  {/if}
</div>
