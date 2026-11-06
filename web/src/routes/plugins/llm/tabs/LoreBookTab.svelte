<script lang="ts">
  import { errorText } from '$lib/errorText';
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
    id: number;
    member: DiscordMember;
    suggestion: string;
    suggestion_by: DiscordMember;
    votes_by_user_id: Record<string, SuggestionVote>;
    // Append suggestions are added to the lore when accepted instead of replacing it
    kind: 'Replace' | 'Append';
  };

  type SuggestionVote = 'Up' | 'Down' | 'Omegalul';

  type Filter = 'all' | 'with_suggestions' | 'with_lore' | 'without_lore';

  // Same limit as /add-user-context
  const MAX_ADDITION_CHARS = 500;
  const DEFAULT_AVATAR = 'https://cdn.discordapp.com/embed/avatars/0.png';
  const OMEGALUL = 'https://cdn.discordapp.com/emojis/373119675829190656.webp?size=48';
  const VOTES: { vote: SuggestionVote; label: string }[] = [
    { vote: 'Up', label: '👍' },
    { vote: 'Down', label: '👎' },
    { vote: 'Omegalul', label: '' }
  ];

  let data = $state<Data | null>(null);
  // Sort order from the first load: cards don't jump around when someone suggests or votes
  let rankByUser = $state<Record<string, number>>({});
  let loadError = $state<string | null>(null);
  let searchQuery = $state('');
  let filter = $state<Filter>('all');
  let isAdmin = $state(false);
  // Per user id: the fact being typed, the rewrite being edited (only while editing), the last error
  let additions = $state<Record<string, string>>({});
  let rewrites = $state<Record<string, string>>({});
  let errors = $state<Record<string, string>>({});

  let currentUserId = $derived(authStore.user?.user_id ?? '');

  async function load(): Promise<void> {
    const res = await fetch('/plugin-api/llm/lore');
    if (!res.ok) {
      loadError = await errorText(res);
      return;
    }
    const loaded: Data = await res.json();
    rankByUser = Object.fromEntries(sortedUserIds(loaded).map((userId, rank) => [userId, rank]));
    data = loaded;
  }
  load();

  /** Suggestions first, then people with lore, then by name */
  function sortedUserIds(data: Data): string[] {
    const suggestionCount = (userId: string) => data.suggestions_by_user[userId]?.length ?? 0;
    return Object.entries(data.lore_by_user)
      .toSorted(
        ([aId, a], [bId, b]) =>
          suggestionCount(bId) - suggestionCount(aId) ||
          Number(hasLore(b)) - Number(hasLore(a)) ||
          a.member.username.localeCompare(b.member.username)
      )
      .map(([userId]) => userId);
  }

  // The plugin decides who the admin is (LLM_ADMIN_USER_ID), the settings tell us if it's us.
  // The accept endpoint checks this again server side
  async function loadIsAdmin(): Promise<void> {
    const res = await fetch('/plugin-api/llm/settings');
    if (!res.ok) return;
    const settings: { allowed_to_edit?: boolean } = await res.json();
    isAdmin = settings.allowed_to_edit === true;
  }
  loadIsAdmin();

  /** Every lore endpoint answers with the whole lore book */
  async function post(path: string, body: object, userId: string): Promise<boolean> {
    delete errors[userId];
    const res = await fetch(`/plugin-api/llm/lore/${path}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body)
    });
    if (!res.ok) {
      errors[userId] = await errorText(res);
      return false;
    }
    data = await res.json();
    return true;
  }

  async function suggestAddition(userId: string) {
    const suggestion = additions[userId]?.trim();
    if (!suggestion) return;
    const body = { for_user_id: userId, suggestion, kind: 'Append' };
    if (await post('suggest', body, userId)) delete additions[userId];
  }

  async function suggestRewrite(userId: string) {
    const suggestion = rewrites[userId]?.trim();
    if (!suggestion) return;
    const body = { for_user_id: userId, suggestion, kind: 'Replace' };
    if (await post('suggest', body, userId)) delete rewrites[userId];
  }

  function vote(userId: string, sug: UserLoreSuggestion, vote: SuggestionVote) {
    post('vote', { for_user_id: userId, suggestion_id: sug.id, vote }, userId);
  }

  function accept(userId: string, sug: UserLoreSuggestion) {
    post('accept', { for_user_id: userId, suggestion_id: sug.id }, userId);
  }

  function voteCount(sug: UserLoreSuggestion, vote: SuggestionVote): number {
    return Object.values(sug.votes_by_user_id).filter((v) => v === vote).length;
  }

  const hasLore = (lore: UserLore) => lore.lore.trim().length > 0;

  let entries = $derived.by(() => {
    if (data == null) return [];
    const suggestionsByUser = data.suggestions_by_user;
    const query = searchQuery.trim().toLowerCase();
    return Object.entries(data.lore_by_user)
      .map(([userId, lore]) => ({ userId, lore, suggestions: suggestionsByUser[userId] ?? [] }))
      .filter(
        ({ lore }) =>
          lore.member.username.toLowerCase().includes(query) ||
          lore.lore.toLowerCase().includes(query)
      )
      .filter(({ lore, suggestions }) => {
        if (filter === 'with_suggestions') return suggestions.length > 0;
        if (filter === 'with_lore') return hasLore(lore);
        if (filter === 'without_lore') return !hasLore(lore);
        return true;
      })
      .toSorted((a, b) => (rankByUser[a.userId] ?? Infinity) - (rankByUser[b.userId] ?? Infinity));
  });

  let peopleWithLore = $derived(Object.values(data?.lore_by_user ?? {}).filter(hasLore).length);
  let pendingCount = $derived(
    Object.values(data?.suggestions_by_user ?? {}).reduce((count, s) => count + s.length, 0)
  );
</script>

{#snippet avatar(member: DiscordMember, size: string)}
  <img
    src={member.avatar_url ?? DEFAULT_AVATAR}
    alt=""
    loading="lazy"
    class="{size} shrink-0 rounded-full"
  />
{/snippet}

<div class="flex w-full flex-col gap-4 text-left">
  <p class="text-xs text-gray-500">
    Facts about people that the bot uses for flavor in its answers. Anyone can suggest a fact or a
    rewrite, everyone votes, and the admin accepts. Every night the bot also suggests facts from the
    day's messages.
  </p>

  <div class="flex flex-col gap-2 sm:flex-row sm:items-center">
    <div class="relative w-full sm:max-w-sm">
      <span
        class="absolute inset-y-0 left-3 my-auto icon-[mdi--magnify] size-4 text-gray-500"
        aria-hidden="true"
      ></span>
      <input
        type="search"
        bind:value={searchQuery}
        placeholder="Search people or lore…"
        class="w-full rounded-lg border border-gray-700 bg-gray-900 py-2 pr-3 pl-9 text-sm text-white outline-none focus:border-indigo-500"
      />
    </div>

    <div class="flex items-center gap-3 sm:ml-auto">
      {#if data}
        <span class="text-xs text-gray-500">
          {peopleWithLore} people with lore ·
          <button
            onclick={() => (filter = 'with_suggestions')}
            class="cursor-pointer {pendingCount > 0 ? 'text-amber-300 hover:text-amber-200' : ''}"
          >
            {pendingCount} pending {pendingCount === 1 ? 'suggestion' : 'suggestions'}
          </button>
        </span>
      {/if}
      <select
        bind:value={filter}
        aria-label="Filter"
        class="rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-sm text-white outline-none focus:border-indigo-500"
      >
        <option value="all">Everyone</option>
        <option value="with_suggestions">With suggestions</option>
        <option value="with_lore">With lore</option>
        <option value="without_lore">Without lore</option>
      </select>
    </div>
  </div>

  {#if loadError}
    <div class="text-sm text-red-400">⚠ {loadError}</div>
  {:else if data == null}
    <div class="animate-pulse text-sm text-gray-400">Reading the archives...</div>
  {:else if entries.length === 0}
    <div class="text-sm text-gray-400">Nobody matches.</div>
  {:else}
    <div class="grid items-start gap-3 lg:grid-cols-2">
      {#each entries as { userId, lore, suggestions } (userId)}
        {@const isMe = userId === currentUserId}
        {@const name = lore.member.username}
        <article class="flex flex-col gap-3 rounded-lg bg-gray-900 p-4">
          <header class="flex items-center gap-3">
            {@render avatar(lore.member, 'size-9')}
            <div class="flex min-w-0 flex-1 flex-col">
              <h3 class="truncate text-sm font-semibold text-gray-100">
                {name}
                {#if isMe}<span class="ml-1 text-xs font-normal text-gray-500">(you)</span>{/if}
              </h3>
              {#if suggestions.length > 0}
                <span class="text-xs text-amber-300">
                  {suggestions.length} pending {suggestions.length === 1
                    ? 'suggestion'
                    : 'suggestions'}
                </span>
              {/if}
            </div>
            {#if !isMe && hasLore(lore) && !(userId in rewrites)}
              <button
                onclick={() => (rewrites[userId] = lore.lore)}
                title="Suggest a new version of the whole lore"
                class="btn-ghost"
              >
                <span class="icon-[mdi--pencil-outline] size-3.5" aria-hidden="true"></span>
                Rewrite
              </button>
            {/if}
          </header>

          {#if userId in rewrites}
            <form
              onsubmit={(e) => {
                e.preventDefault();
                suggestRewrite(userId);
              }}
              class="flex flex-col gap-2"
            >
              <textarea
                bind:value={rewrites[userId]}
                aria-label="New lore for {name}"
                class="w-full rounded border border-gray-700 bg-gray-950 p-2 text-sm text-white outline-none focus:border-indigo-500"
              ></textarea>
              <div class="flex justify-end gap-2">
                <button type="button" onclick={() => delete rewrites[userId]} class="btn-ghost">
                  Cancel
                </button>
                <button
                  type="submit"
                  disabled={!rewrites[userId].trim() ||
                    rewrites[userId].trim() === lore.lore.trim()}
                  class="btn-primary"
                >
                  Suggest rewrite
                </button>
              </div>
            </form>
          {:else if hasLore(lore)}
            <p class="text-sm leading-relaxed break-words whitespace-pre-wrap text-gray-200">
              {lore.lore}
            </p>
          {:else}
            <p class="text-sm text-gray-500 italic">No lore yet.</p>
          {/if}

          {#if isMe}
            <p class="text-xs text-gray-500">Others can suggest facts about you here.</p>
          {:else}
            <form
              onsubmit={(e) => {
                e.preventDefault();
                suggestAddition(userId);
              }}
              class="flex gap-2"
            >
              <input
                bind:value={additions[userId]}
                maxlength={MAX_ADDITION_CHARS}
                placeholder="Add a fact about {name}…"
                aria-label="New fact about {name}"
                class="min-w-0 flex-1 rounded border border-gray-700 bg-gray-950 px-2 py-1.5 text-sm text-white outline-none focus:border-indigo-500"
              />
              <button type="submit" disabled={!additions[userId]?.trim()} class="btn-primary">
                <span class="icon-[mdi--plus] size-4" aria-hidden="true"></span>
                Suggest
              </button>
            </form>
          {/if}

          {#if errors[userId]}
            <p class="text-xs text-red-400">⚠ {errors[userId]}</p>
          {/if}

          {#each suggestions as sug (sug.id)}
            {@const isAddition = sug.kind === 'Append'}
            {@const isMine = sug.suggestion_by.user_id === currentUserId}
            <div
              class="flex flex-col gap-2 rounded-md border-l-2 bg-gray-950 px-3 py-2 {isAddition
                ? 'border-green-500'
                : 'border-amber-400'}"
            >
              <div class="flex flex-wrap items-center gap-1.5 text-xs text-gray-400">
                <span class="font-medium {isAddition ? 'text-green-400' : 'text-amber-300'}">
                  {isAddition ? '➕ Addition' : '✏️ Rewrite'}
                </span>
                by
                {@render avatar(sug.suggestion_by, 'size-4')}
                <span class="text-gray-300">{isMine ? 'you' : sug.suggestion_by.username}</span>
              </div>
              <p class="text-sm break-words whitespace-pre-wrap text-gray-200">{sug.suggestion}</p>
              <div class="flex flex-wrap items-center gap-1">
                {#each VOTES as { vote: kind, label } (kind)}
                  {@const voted = sug.votes_by_user_id[currentUserId] === kind}
                  <button
                    onclick={() => vote(userId, sug, kind)}
                    disabled={isMine}
                    title={isMine ? "You can't vote on your own suggestion" : kind}
                    aria-pressed={voted}
                    class="btn-vote {voted ? 'bg-indigo-500/30! ring-1 ring-indigo-400' : ''}"
                  >
                    {#if label}{label}{:else}<img
                        class="size-4"
                        alt="Omegalul"
                        src={OMEGALUL}
                      />{/if}
                    <span class="tabular-nums">{voteCount(sug, kind)}</span>
                  </button>
                {/each}
                {#if isAdmin}
                  <button
                    onclick={() => accept(userId, sug)}
                    class="btn-vote ml-auto bg-green-600! text-white hover:bg-green-500!"
                  >
                    Accept
                  </button>
                {/if}
              </div>
            </div>
          {/each}
        </article>
      {/each}
    </div>
  {/if}
</div>

<style>
  @reference "../../../layout.css";

  .btn-primary {
    @apply flex cursor-pointer items-center gap-1 rounded bg-indigo-600 px-3 py-1 text-xs font-medium text-white transition-colors hover:bg-indigo-500 disabled:cursor-not-allowed disabled:opacity-40 disabled:hover:bg-indigo-600;
  }

  .btn-ghost {
    @apply flex cursor-pointer items-center gap-1 rounded px-2 py-1 text-xs text-gray-400 transition-colors hover:bg-gray-800 hover:text-white;
  }

  .btn-vote {
    @apply flex cursor-pointer items-center gap-1 rounded bg-gray-800 px-2 py-1 text-xs transition-colors hover:bg-gray-700 disabled:cursor-not-allowed disabled:opacity-50;
  }

  textarea {
    field-sizing: content;
    min-height: 5rem;
  }
</style>
