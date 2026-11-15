<script lang="ts">
  import { errorText } from '$lib/errorText';
  import { authStore, type DiscordMember } from '../../../../lib/stores/auth.svelte';

  type Data = {
    current: string;
    // Sorted by name
    personalities: { name: string; prompt: string }[];
    suggestions: PersonalitySuggestion[];
    is_admin: boolean;
  };

  type PersonalityChange =
    | { kind: 'Create'; prompt: string }
    | { kind: 'Edit'; prompt: string }
    | { kind: 'Delete' };

  type PersonalitySuggestion = {
    id: number;
    name: string;
    change: PersonalityChange;
    suggestion_by: DiscordMember;
    suggested_at: number;
    votes_by_user_id: Record<string, SuggestionVote>;
  };

  type SuggestionVote = 'Up' | 'Down' | 'Omegalul';

  // Same limits as the plugin
  const MAX_NAME_CHARS = 32;
  const MAX_PROMPT_CHARS = 10_000;
  const DEFAULT_AVATAR = 'https://cdn.discordapp.com/embed/avatars/0.png';
  const OMEGALUL = 'https://cdn.discordapp.com/emojis/373119675829190656.webp?size=48';
  const VOTES: { vote: SuggestionVote; label: string }[] = [
    { vote: 'Up', label: '👍' },
    { vote: 'Down', label: '👎' },
    { vote: 'Omegalul', label: '' }
  ];
  const CHANGES = {
    Create: { label: '➕ New personality', border: 'border-green-500', text: 'text-green-400' },
    Edit: { label: '✏️ Edit', border: 'border-amber-400', text: 'text-amber-300' },
    Delete: { label: '🗑️ Deletion', border: 'border-red-500', text: 'text-red-400' }
  };

  let data = $state<Data | null>(null);
  let loadError = $state<string | null>(null);
  let switchStatus = $state<'idle' | 'saving' | 'saved' | `error:${string}`>('idle');
  // Per personality name: its prompt being edited, only while editing
  let edits = $state<Record<string, string>>({});
  let newName = $state('');
  let newPrompt = $state('');
  // Per personality name ('' for the new personality form): the last error
  let errors = $state<Record<string, string>>({});

  let currentUserId = $derived(authStore.user?.user_id ?? '');
  let current = $derived(data?.personalities.find((p) => p.name === data?.current));
  let newSuggestions = $derived(data?.suggestions.filter((s) => s.change.kind === 'Create') ?? []);

  async function load(): Promise<void> {
    const res = await fetch('/plugin-api/llm/personalities');
    if (!res.ok) {
      loadError = await errorText(res);
      return;
    }
    data = await res.json();
  }
  load();

  /** Every personality endpoint answers with the whole state */
  async function post(path: string, body: object, errorKey: string): Promise<boolean> {
    delete errors[errorKey];
    const res = await fetch(`/plugin-api/llm/${path}`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(body)
    });
    if (!res.ok) {
      errors[errorKey] = await errorText(res);
      return false;
    }
    data = await res.json();
    return true;
  }

  async function switchTo(name: string) {
    switchStatus = 'saving';
    delete errors.switch;
    if (await post('personality', { name }, 'switch')) {
      switchStatus = 'saved';
      setTimeout(() => {
        if (switchStatus === 'saved') switchStatus = 'idle';
      }, 2000);
    } else {
      switchStatus = `error:${errors.switch}`;
      await load();
    }
  }

  async function suggestNew() {
    const body = { name: newName.trim(), change: { kind: 'Create', prompt: newPrompt } };
    if (await post('personality/suggest', body, '')) {
      newName = '';
      newPrompt = '';
    }
  }

  async function suggestEdit(name: string) {
    const body = { name, change: { kind: 'Edit', prompt: edits[name] } };
    if (await post('personality/suggest', body, name)) delete edits[name];
  }

  function suggestDeletion(name: string) {
    post('personality/suggest', { name, change: { kind: 'Delete' } }, name);
  }

  function vote(sug: PersonalitySuggestion, vote: SuggestionVote) {
    post('personality/vote', { suggestion_id: sug.id, vote }, errorKey(sug));
  }

  function accept(sug: PersonalitySuggestion) {
    post('personality/accept', { suggestion_id: sug.id }, errorKey(sug));
  }

  /** Withdraws our own suggestion, or rejects one as the admin */
  function dismiss(sug: PersonalitySuggestion) {
    post('personality/dismiss', { suggestion_id: sug.id }, errorKey(sug));
  }

  /** Errors about new personalities show under the form, the others under their personality */
  const errorKey = (sug: PersonalitySuggestion) => (sug.change.kind === 'Create' ? '' : sug.name);

  const suggestionsFor = (name: string) =>
    data?.suggestions.filter((s) => s.name === name && s.change.kind !== 'Create') ?? [];

  const voteCount = (sug: PersonalitySuggestion, vote: SuggestionVote) =>
    Object.values(sug.votes_by_user_id).filter((v) => v === vote).length;

  const formatDate = (secs: number) =>
    new Date(secs * 1000).toLocaleDateString(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric'
    });
</script>

{#snippet error(key: string)}
  {#if errors[key]}<p class="text-xs text-red-400">⚠ {errors[key]}</p>{/if}
{/snippet}

{#snippet suggestionCard(sug: PersonalitySuggestion, oldPrompt: string | undefined)}
  {@const isMine = sug.suggestion_by.user_id === currentUserId}
  {@const style = CHANGES[sug.change.kind]}
  <div class="flex flex-col gap-2 rounded-md border-l-2 bg-gray-950 px-3 py-2 {style.border}">
    <div class="flex flex-wrap items-center gap-1.5 text-xs text-gray-400">
      <span class="font-medium {style.text}">{style.label}</span>
      {#if sug.change.kind === 'Create'}
        <code class="text-gray-200">{sug.name}</code>
      {/if}
      by
      <img
        src={sug.suggestion_by.avatar_url ?? DEFAULT_AVATAR}
        alt=""
        loading="lazy"
        class="size-4 shrink-0 rounded-full"
      />
      <span class="text-gray-300">{isMine ? 'you' : sug.suggestion_by.username}</span>
      <span>· {formatDate(sug.suggested_at)}</span>
    </div>
    {#if sug.change.kind !== 'Delete'}
      {#if oldPrompt != null}
        <details class="text-xs text-gray-500">
          <summary class="cursor-pointer">Current prompt</summary>
          <p class="mt-1 break-words whitespace-pre-wrap line-through">{oldPrompt}</p>
        </details>
      {/if}
      <p class="max-h-96 overflow-y-auto text-sm break-words whitespace-pre-wrap text-gray-200">
        {sug.change.prompt}
      </p>
    {/if}
    <div class="flex flex-wrap items-center gap-1">
      {#each VOTES as { vote: kind, label } (kind)}
        {@const voted = sug.votes_by_user_id[currentUserId] === kind}
        <button
          onclick={() => vote(sug, kind)}
          disabled={isMine}
          title={isMine ? "You can't vote on your own suggestion" : kind}
          aria-pressed={voted}
          class="btn-vote {voted ? 'bg-indigo-500/30! ring-1 ring-indigo-400' : ''}"
        >
          {#if label}{label}{:else}<img class="size-4" alt="Omegalul" src={OMEGALUL} />{/if}
          <span class="tabular-nums">{voteCount(sug, kind)}</span>
        </button>
      {/each}
      <span class="ml-auto flex gap-1">
        {#if isMine || data?.is_admin}
          <button onclick={() => dismiss(sug)} class="btn-ghost">
            {isMine ? 'Withdraw' : 'Reject'}
          </button>
        {/if}
        {#if data?.is_admin}
          <button
            onclick={() => accept(sug)}
            class="btn-vote bg-green-600! text-white hover:bg-green-500!"
          >
            Accept
          </button>
        {/if}
      </span>
    </div>
  </div>
{/snippet}

<div class="flex w-full flex-col gap-4 text-left">
  <p class="text-xs text-gray-500">
    The bot's personality is the start of its system prompt. Anyone can switch it, and suggest a new
    personality, an edit or a deletion: everyone votes, and the admin accepts. New personalities
    show up in <code>/llm personality set</code> only once they're added to the bot's code.
  </p>

  {#if loadError}
    <div class="text-sm text-red-400">⚠ {loadError}</div>
  {:else if data == null}
    <div class="animate-pulse text-sm text-gray-400">Loading personalities...</div>
  {:else}
    <section class="flex flex-col gap-2 rounded-lg bg-gray-900 p-4">
      <div class="flex flex-col gap-2 sm:flex-row sm:items-center">
        <label for="personality" class="text-sm font-medium text-gray-400">Active personality</label
        >
        <select
          id="personality"
          value={data.current}
          onchange={(e) => switchTo(e.currentTarget.value)}
          class="rounded border border-gray-700 bg-gray-950 p-2 text-sm text-white outline-none focus:border-indigo-500 sm:w-64"
        >
          {#each data.personalities as { name } (name)}
            <option value={name}>{name}</option>
          {/each}
        </select>
        <span class="h-5 text-xs">
          {#if switchStatus === 'saving'}
            <span class="animate-pulse text-indigo-400">Switching...</span>
          {:else if switchStatus === 'saved'}
            <span class="text-green-500">✓ Switched</span>
          {:else if switchStatus.startsWith('error')}
            <span class="text-red-500">⚠ {switchStatus.slice(6)}</span>
          {/if}
        </span>
      </div>
      {#if current}
        <details>
          <summary class="cursor-pointer text-xs text-gray-500">Its prompt</summary>
          <p class="mt-2 text-sm break-words whitespace-pre-wrap text-gray-300">{current.prompt}</p>
        </details>
      {/if}
    </section>

    <div class="grid items-start gap-3 lg:grid-cols-2">
      {#each data.personalities as { name, prompt } (name)}
        {@const isActive = name === data.current}
        <article class="flex flex-col gap-3 rounded-lg bg-gray-900 p-4">
          <header class="flex items-center gap-2">
            <h3 class="text-sm font-semibold text-gray-100">{name}</h3>
            {#if isActive}
              <span class="rounded bg-indigo-500/20 px-1.5 text-xs text-indigo-300">active</span>
            {/if}
            {#if !(name in edits)}
              <span class="ml-auto flex gap-1">
                <button onclick={() => (edits[name] = prompt)} class="btn-ghost">
                  Suggest edit
                </button>
                <button
                  onclick={() => suggestDeletion(name)}
                  disabled={isActive}
                  title={isActive ? 'Switch to another personality first' : ''}
                  class="btn-ghost disabled:cursor-not-allowed disabled:opacity-40"
                >
                  Suggest deletion
                </button>
              </span>
            {/if}
          </header>

          {#if name in edits}
            <form
              onsubmit={(e) => {
                e.preventDefault();
                suggestEdit(name);
              }}
              class="flex flex-col gap-2"
            >
              <textarea
                bind:value={edits[name]}
                maxlength={MAX_PROMPT_CHARS}
                aria-label="New prompt for {name}"
                class="w-full rounded border border-gray-700 bg-gray-950 p-2 text-sm text-white outline-none focus:border-indigo-500"
              ></textarea>
              <div class="flex justify-end gap-2">
                <button type="button" onclick={() => delete edits[name]} class="btn-ghost">
                  Cancel
                </button>
                <button
                  type="submit"
                  disabled={!edits[name].trim() || edits[name].trim() === prompt}
                  class="btn-primary"
                >
                  Suggest edit
                </button>
              </div>
            </form>
          {:else}
            <details>
              <summary class="cursor-pointer text-xs text-gray-500">Prompt</summary>
              <p class="mt-2 text-sm break-words whitespace-pre-wrap text-gray-300">{prompt}</p>
            </details>
          {/if}

          {@render error(name)}
          {#each suggestionsFor(name) as sug (sug.id)}
            {@render suggestionCard(sug, sug.change.kind === 'Edit' ? prompt : undefined)}
          {/each}
        </article>
      {/each}

      <article class="flex flex-col gap-3 rounded-lg bg-gray-900 p-4">
        <h3 class="text-sm font-semibold text-gray-100">New personality</h3>
        <form
          onsubmit={(e) => {
            e.preventDefault();
            suggestNew();
          }}
          class="flex flex-col gap-2"
        >
          <input
            bind:value={newName}
            maxlength={MAX_NAME_CHARS}
            placeholder="name, like new-yorker"
            aria-label="Name of the new personality"
            class="rounded border border-gray-700 bg-gray-950 p-2 text-sm text-white outline-none focus:border-indigo-500"
          />
          <textarea
            bind:value={newPrompt}
            maxlength={MAX_PROMPT_CHARS}
            placeholder="You are…"
            aria-label="Prompt of the new personality"
            class="w-full rounded border border-gray-700 bg-gray-950 p-2 text-sm text-white outline-none focus:border-indigo-500"
          ></textarea>
          <div class="flex justify-end">
            <button
              type="submit"
              disabled={!newName.trim() || !newPrompt.trim()}
              class="btn-primary"
            >
              Suggest
            </button>
          </div>
        </form>
        {@render error('')}
        {#each newSuggestions as sug (sug.id)}
          {@render suggestionCard(sug, undefined)}
        {/each}
      </article>
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
    min-height: 8rem;
    max-height: 32rem;
  }
</style>
