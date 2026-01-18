<script lang="ts">
  import { authStore, type DiscordMember } from '../../../../lib/stores/auth.svelte';

  type Data = {
    lore_by_user: Record<string, UserLore>;
    suggestions_by_user: Record<string, UserLoreSuggestion[]>;
  };

  type UserLore = {
    member: DiscordMember;
    lore: string;
  };

  type UserLoreSuggestion = {
    member: DiscordMember;
    suggestion: string;
    suggestion_by: DiscordMember;
    votes_by_user_id: Record<string, SuggestionVote>;
  };

  type SuggestionVote = 'Up' | 'Down' | 'Omegalul';

  let loreForms = $state<Record<string, string>>({});

  async function loadData(): Promise<Data> {
    const res = await fetch('/plugin-api/llm/lore');
    const data: Data = await res.json();
    init(data);
    return data;
  }

  function init(data: Data): void {
    loreForms = {};
    // Initialize the "scratchpad" for edits
    for (const [userId, userLore] of Object.entries(data.lore_by_user)) {
      if (!(userId in loreForms)) loreForms[userId] = userLore.lore;
    }
  }

  let initPromise = $state(loadData());
  let currentUserId = $derived(authStore.user?.user_id ?? '');
  let searchQuery = $state('');

  async function handleSuggest(userId: string) {
    const suggestion = loreForms[userId];
    if (!suggestion?.trim()) return;

    const resp = await fetch('/plugin-api/llm/lore/suggest', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ for_user_id: userId, suggestion: suggestion.trim() })
    });
    if (!resp.ok) {
      alert('Failed to submit suggestion: ' + (await resp.text()));
      return;
    }
    const data = await resp.json();
    initPromise = Promise.resolve(data);
    init(data);
  }

  async function handleVote(for_user_id: string, by_user_id: string, vote: SuggestionVote) {
    const resp = await fetch('/plugin-api/llm/lore/vote', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ for_user_id, by_user_id, vote })
    });
    if (!resp.ok) {
      alert('Failed to submit vote: ' + (await resp.text()));
      return;
    }
    const data = await resp.json();
    initPromise = Promise.resolve(data);
    init(data);
  }

  function totalVotes(sug: UserLoreSuggestion, voteType: SuggestionVote): number {
    let total = 0;
    for (const vote of Object.values(sug.votes_by_user_id)) {
      if (vote === voteType) total++;
    }
    return total;
  }
</script>

{#snippet memberInfo(member: DiscordMember, isSmall = false)}
  <div class="flex items-center gap-3 {isSmall ? 'pl-8 opacity-80' : ''} max-w-3xs">
    <img src={member.avatar_url} alt="" class="{isSmall ? 'h-6 w-6' : 'h-8 w-8'} rounded-md" />
    <span class="{isSmall ? 'text-xs' : 'text-sm font-semibold'} truncate">
      {member.username}
    </span>
  </div>
{/snippet}

<div class="mb-6 flex items-center gap-2">
  <div class="relative w-full max-w-sm">
    <span class="absolute inset-y-0 left-3 flex items-center text-gray-500"> 🔍 </span>
    <input
      type="text"
      bind:value={searchQuery}
      placeholder="Search users..."
      class="w-full rounded-lg border border-gray-700 bg-gray-900 py-2 pr-4 pl-10 text-sm text-white outline-none focus:border-indigo-500"
    />
  </div>

  {#if searchQuery}
    <button
      onclick={() => (searchQuery = '')}
      class="cursor-pointer text-xs text-gray-500 hover:text-white"
    >
      Clear
    </button>
  {/if}
</div>

{#await initPromise}
  <div class="animate-pulse text-gray-400">Reading the archives...</div>
{:then data}
  <div class="grid-container">
    {#each Object.entries(data.lore_by_user).filter(([_, val]) => val.member.username
        .toLowerCase()
        .includes(searchQuery.toLowerCase())) as [userId, userLore]}
      <div class="contents">
        {@render memberInfo(userLore.member)}

        <textarea
          rows="2"
          placeholder="No lore yet"
          disabled={userId === currentUserId}
          bind:value={loreForms[userId]}
          class="w-full rounded border border-gray-700 bg-gray-900 p-2 text-sm text-white outline-none focus:border-indigo-500 disabled:cursor-not-allowed disabled:opacity-50"
        ></textarea>

        <button
          onclick={() => handleSuggest(userId)}
          class="rounded bg-indigo-600 px-3 py-1 text-xs font-medium text-white transition-all hover:bg-indigo-500 disabled:opacity-0"
          disabled={loreForms[userId] === userLore.lore}
        >
          Suggest Change
        </button>
      </div>

      {#if data.suggestions_by_user[userId]}
        {#each data.suggestions_by_user[userId] as sug}
          <div class="contents">
            <div class="flex">{@render memberInfo(sug.suggestion_by, true)}</div>

            <div
              class="rounded border border-dashed border-gray-700 bg-gray-950 p-2 text-sm text-gray-300 italic"
            >
              {sug.suggestion}
            </div>

            <div class="flex gap-1">
              <button
                onclick={() => handleVote(userId, sug.suggestion_by.user_id, 'Up')}
                disabled={sug.suggestion_by.user_id === currentUserId}
                class={[
                  'btn-vote',
                  sug.votes_by_user_id[currentUserId] === 'Up' ? 'bg-gray-700!' : ''
                ]}
                title="Upvote">👍 {totalVotes(sug, 'Up')}</button
              >
              <button
                onclick={() => handleVote(userId, sug.suggestion_by.user_id, 'Down')}
                disabled={sug.suggestion_by.user_id === currentUserId}
                class={[
                  'btn-vote',
                  sug.votes_by_user_id[currentUserId] === 'Down' ? 'bg-gray-700!' : ''
                ]}
                title="Downvote">👎 {totalVotes(sug, 'Down')}</button
              >
              <button
                onclick={() => handleVote(userId, sug.suggestion_by.user_id, 'Omegalul')}
                disabled={sug.suggestion_by.user_id === currentUserId}
                class={[
                  'btn-vote',
                  sug.votes_by_user_id[currentUserId] === 'Omegalul' ? 'bg-gray-700!' : ''
                ]}
              >
                <span class="flex gap-1">
                  <img
                    class="size-4"
                    alt="Omegalul"
                    src="https://cdn.discordapp.com/emojis/373119675829190656.webp?size=240"
                  />
                  {totalVotes(sug, 'Omegalul')}
                </span>
              </button>
            </div>
          </div>
        {/each}
      {/if}

      <div class="col-span-3 my-2 border-b border-gray-800"></div>
    {/each}
  </div>
{/await}

<style>
  @reference "../../../layout.css";

  .btn-vote {
    @apply cursor-pointer rounded bg-gray-800 px-2 py-1 text-xs transition-colors hover:bg-gray-700 disabled:cursor-not-allowed disabled:opacity-50;
  }

  /* Helper to treat wrappers as transparent for the grid */
  .contents {
    display: contents;
  }

  .grid-container {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    column-gap: 1rem;
    row-gap: 0.5rem;
    width: 100%;
  }

  /* Mobile: Switch to a single column card layout */
  @media (max-width: 640px) {
    .grid-container {
      grid-template-columns: 1fr; /* Everything takes full width */
      row-gap: 1rem; /* More space between stacked elements */
    }

    /* Force the "spacer" line to be more prominent between 'cards' */
    .col-span-3 {
      grid-column: 1 / -1;
      margin-top: 1.5rem;
      border-bottom-width: 2px;
    }

    /* Optional: Make the suggestion row look indented on mobile */
    .pl-8 {
      padding-left: 1.5rem;
    }
  }
</style>
