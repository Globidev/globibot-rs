<script lang="ts">
  import DiscordLogin from '../../../lib/components/DiscordLogin.svelte';
  import { authStore } from '../../../lib/stores/auth.svelte';

  import GeneralSettingsTab from './tabs/GeneralSettingsTab.svelte';
  import PersonalityTab from './tabs/PersonalityTab.svelte';
  import LoreBookTab from './tabs/LoreBookTab.svelte';
  import ContributorsTab from './tabs/ContributorsTab.svelte';

  const tabs = [
    { id: 'general_settings', label: '⚙️ General settings', component: GeneralSettingsTab },
    { id: 'personality', label: '🎭 Personality', component: PersonalityTab },
    { id: 'lore_book', label: '📓 Lore book', component: LoreBookTab },
    { id: 'contributors', label: '❤️ Contributors', component: ContributorsTab }
  ] as const;

  type TabId = (typeof tabs)[number]['id'];
  const tabIds: string[] = tabs.map((t) => t.id);

  let storedId = window.location.hash.slice(1);
  let defaultId = tabIds.includes(storedId) ? (storedId as TabId) : 'general_settings';
  let activeTabId = $state(defaultId);
  let ActiveTab = $derived(tabs.find((t) => t.id === activeTabId)?.component);

  async function fetchData(url: string) {
    const res = await fetch(url);
    if (!res.ok) throw new Error('Failed to fetch data');
    return res.json();
  }
</script>

<div class="mx-auto max-w-5xl px-6">
  <!-- Header -->
  <header class="mb-12 text-center">
    <h1 class="text-2xl font-semibold tracking-tight">LLM plugin</h1>
  </header>

  <div class="flex flex-col items-center gap-6 text-center">
    {#if !authStore.initialized}
      <div class="text-sm">Loading authentication status...</div>
    {:else if authStore.user == null}
      <div class="text-sm">You must be logged in to access the LLM plugin.</div>
      <DiscordLogin />
    {:else}
      <div class="mb-6 flex border-b border-gray-700">
        {#each tabs as tab}
          <button
            onclick={() => {
              activeTabId = tab.id;
              location.hash = tab.id;
            }}
            class="-mb-px cursor-pointer px-4 py-2 transition-colors {activeTabId === tab.id
              ? 'border-b-2 border-indigo-500 text-indigo-400'
              : 'text-gray-400 hover:text-white'}"
          >
            {tab.label}
          </button>
        {/each}
      </div>

      <div class="flex w-full flex-1 flex-col rounded-lg bg-gray-800 p-4">
        {#if ActiveTab}
          <ActiveTab />
        {/if}
      </div>
    {/if}
  </div>
</div>
