<script lang="ts">
  import { errorText } from '$lib/errorText';
  import { authStore, type DiscordMember } from '../../../../lib/stores/auth.svelte';

  type Data = {
    current: string;
    // Sorted by name
    personalities: Personality[];
    suggestions: PersonalitySuggestion[];
    is_admin: boolean;
  };

  type Personality = {
    name: string;
    prompt: string;
    // The bot's nickname and avatar in the server while it's active. null: its global ones.
    nickname: string | null;
    avatar_version: string | null;
  };

  type AvatarChange = { kind: 'Keep' } | { kind: 'Remove' } | { kind: 'Set'; data_uri: string };

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
  const MAX_NICKNAME_CHARS = 32;
  // Uploads are cropped to a square and resized to at most this, as PNG
  const AVATAR_SIZE = 512;
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
  // Per personality name: its nickname and avatar being edited (admin only)
  let profileEdits = $state<Record<string, { nickname: string; avatar: AvatarChange }>>({});
  let applyStatus = $state<'idle' | 'applying' | 'applied'>('idle');
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

  const avatarUrl = (p: Personality) =>
    p.avatar_version &&
    `/plugin-api/llm/personality/avatar?name=${encodeURIComponent(p.name)}&v=${p.avatar_version}`;

  /** Crops to a centered square and resizes to a PNG. Any image the browser shows, SVG too. */
  async function toAvatarDataUri(file: File): Promise<string> {
    const url = URL.createObjectURL(file);
    try {
      const img = new Image();
      img.src = url;
      await img.decode();
      const side = Math.min(img.naturalWidth, img.naturalHeight);
      const size = Math.min(side, AVATAR_SIZE);
      const canvas = document.createElement('canvas');
      canvas.width = canvas.height = size;
      const x = (img.naturalWidth - side) / 2;
      const y = (img.naturalHeight - side) / 2;
      canvas.getContext('2d')!.drawImage(img, x, y, side, side, 0, 0, size, size);
      return canvas.toDataURL('image/png');
    } finally {
      URL.revokeObjectURL(url);
    }
  }

  async function pickAvatar(name: string, file: File | undefined) {
    if (!file) return;
    delete errors[name];
    try {
      profileEdits[name].avatar = { kind: 'Set', data_uri: await toAvatarDataUri(file) };
    } catch {
      errors[name] = "Couldn't read that image";
    }
  }

  async function saveProfile(name: string) {
    const { nickname, avatar } = profileEdits[name];
    if (await post('personality/profile', { name, nickname, avatar }, name)) {
      delete profileEdits[name];
    } else {
      // Saved even when Discord refused it
      await load();
    }
  }

  async function applyProfile() {
    applyStatus = 'applying';
    const applied = await post('personality/profile/apply', {}, 'switch');
    applyStatus = applied ? 'applied' : 'idle';
    if (applied) {
      setTimeout(() => {
        if (applyStatus === 'applied') applyStatus = 'idle';
      }, 2000);
    }
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
        {#if data.is_admin}
          <button
            onclick={applyProfile}
            disabled={applyStatus === 'applying'}
            title="Sets the bot's nickname and avatar in the server to the active personality's again"
            class="btn-ghost sm:ml-auto"
          >
            {#if applyStatus === 'applying'}Applying...{:else if applyStatus === 'applied'}✓ Applied{:else}Reapply
              nickname & avatar{/if}
          </button>
        {/if}
      </div>
      {#if switchStatus === 'idle' && errors.switch}
        {@render error('switch')}
      {/if}
      {#if current}
        <details>
          <summary class="cursor-pointer text-xs text-gray-500">Its prompt</summary>
          <p class="mt-2 text-sm break-words whitespace-pre-wrap text-gray-300">{current.prompt}</p>
        </details>
      {/if}
    </section>

    <div class="grid items-start gap-3 lg:grid-cols-2">
      {#each data.personalities as personality (personality.name)}
        {@const { name, prompt, nickname } = personality}
        {@const isActive = name === data.current}
        {@const avatar = avatarUrl(personality)}
        <article class="flex flex-col gap-3 rounded-lg bg-gray-900 p-4">
          <header class="flex items-center gap-2">
            {#if avatar}
              <img src={avatar} alt="" loading="lazy" class="size-6 shrink-0 rounded-full" />
            {/if}
            <h3 class="text-sm font-semibold text-gray-100">{name}</h3>
            {#if nickname}
              <span class="truncate text-xs text-gray-400" title="Nickname in the server">
                as {nickname}
              </span>
            {/if}
            {#if isActive}
              <span class="rounded bg-indigo-500/20 px-1.5 text-xs text-indigo-300">active</span>
            {/if}
            {#if !(name in edits)}
              <span class="ml-auto flex gap-1">
                {#if data.is_admin && !(name in profileEdits)}
                  <button
                    onclick={() =>
                      (profileEdits[name] = { nickname: nickname ?? '', avatar: { kind: 'Keep' } })}
                    class="btn-ghost"
                  >
                    Nickname & avatar
                  </button>
                {/if}
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

          {#if name in profileEdits}
            {@const edit = profileEdits[name]}
            <form
              onsubmit={(e) => {
                e.preventDefault();
                saveProfile(name);
              }}
              class="flex flex-col gap-2 rounded-md bg-gray-950 p-3"
            >
              <div class="flex items-center gap-3">
                {#if edit.avatar.kind === 'Set'}
                  <img src={edit.avatar.data_uri} alt="New avatar" class="size-12 rounded-full" />
                {:else if edit.avatar.kind === 'Keep' && avatar}
                  <img src={avatar} alt="Current avatar" class="size-12 rounded-full" />
                {:else}
                  <div
                    class="flex size-12 items-center justify-center rounded-full bg-gray-800 text-[10px] text-gray-500"
                  >
                    global
                  </div>
                {/if}
                <div class="flex flex-col gap-1">
                  <input
                    type="file"
                    accept="image/*"
                    aria-label="Avatar for {name}"
                    onchange={(e) => pickAvatar(name, e.currentTarget.files?.[0])}
                    class="text-xs text-gray-400 file:mr-2 file:cursor-pointer file:rounded file:border-0 file:bg-gray-800 file:px-2 file:py-1 file:text-gray-200"
                  />
                  {#if edit.avatar.kind === 'Set' || (edit.avatar.kind === 'Keep' && avatar)}
                    <button
                      type="button"
                      onclick={() => (edit.avatar = { kind: 'Remove' })}
                      class="btn-ghost self-start"
                    >
                      Use the global avatar
                    </button>
                  {/if}
                </div>
              </div>
              <input
                bind:value={edit.nickname}
                maxlength={MAX_NICKNAME_CHARS}
                placeholder="Nickname (empty: the global name)"
                aria-label="Nickname for {name}"
                class="rounded border border-gray-700 bg-gray-950 p-2 text-sm text-white outline-none focus:border-indigo-500"
              />
              <p class="text-xs text-gray-500">
                {isActive
                  ? 'Applied to the server when saved.'
                  : 'Applied to the server when switching to this personality.'}
              </p>
              <div class="flex justify-end gap-2">
                <button type="button" onclick={() => delete profileEdits[name]} class="btn-ghost">
                  Cancel
                </button>
                <button type="submit" class="btn-primary">Save</button>
              </div>
            </form>
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
