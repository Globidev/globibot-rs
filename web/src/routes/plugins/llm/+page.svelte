<script lang="ts">
  import DiscordLogin from '../../../lib/components/DiscordLogin.svelte';
  import { authStore } from '../../../lib/stores/auth.svelte';

  import GeneralSettingsTab from './tabs/GeneralSettingsTab.svelte';
  import PersonalityTab from './tabs/PersonalityTab.svelte';
  import LoreBookTab from './tabs/LoreBookTab.svelte';
  import ContributorsTab from './tabs/ContributorsTab.svelte';
  import UsageTab from './tabs/UsageTab.svelte';
  import ToolsTab from './tabs/ToolsTab.svelte';
  import RemindersTab from './tabs/RemindersTab.svelte';

  const tabs = [
    { id: 'general_settings', label: '⚙️ General settings', component: GeneralSettingsTab },
    { id: 'personality', label: '🎭 Personality', component: PersonalityTab },
    { id: 'lore_book', label: '📓 Lore book', component: LoreBookTab },
    { id: 'usage', label: '📊 Usage', component: UsageTab },
    { id: 'tools', label: '🔧 Tools', component: ToolsTab },
    { id: 'reminders', label: '⏰ Reminders', component: RemindersTab },
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

<div class="mx-auto max-w-5xl">
  <!-- Header -->
  <header class="mb-6 text-center sm:mb-12">
    <h1 class="text-2xl font-semibold tracking-tight">LLM plugin</h1>
  </header>

  <div class="flex flex-col items-center gap-4 text-center sm:gap-6">
    {#if !authStore.initialized}
      <div class="text-sm">Loading authentication status...</div>
    {:else if authStore.user == null}
      <div class="text-sm">You must be logged in to access the LLM plugin.</div>
      <DiscordLogin />
    {:else}
      <div class="mb-2 flex max-w-full overflow-x-auto border-b border-gray-700 sm:mb-6">
        {#each tabs as tab}
          <button
            {@attach (button) => {
              // The bar scrolls sideways on phones: keep the open tab in sight
              if (tab.id === activeTabId)
                button.scrollIntoView({ block: 'nearest', inline: 'nearest' });
            }}
            onclick={() => {
              activeTabId = tab.id;
              location.hash = tab.id;
            }}
            class="-mb-px shrink-0 cursor-pointer px-3 py-2 whitespace-nowrap transition-colors sm:px-4 {activeTabId ===
            tab.id
              ? 'border-b-2 border-indigo-500 text-indigo-400'
              : 'text-gray-400 hover:text-white'}"
          >
            {tab.label}
          </button>
        {/each}
      </div>

      <div class="flex w-full flex-1 flex-col rounded-lg bg-gray-800 p-3 sm:p-4">
        {#if ActiveTab}
          <ActiveTab />
        {/if}
      </div>
    {/if}
  </div>
</div>
