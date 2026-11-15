<script lang="ts">
  import { errorText } from '$lib/errorText';
  type Data = {
    model: string;
    prompt: string;
    context_window_size: number;
    image_model: string;
    image_generation_enabled: boolean;
    images_per_user_per_day: number;
    context_windows_by_channel: ChannelContextWindow[];
    allowed_to_edit: boolean;
  };

  type ChannelContextWindow = {
    channel_name: string;
    size: number;
  };

  let formData = $state<Data | null>(null);
  let status = $state<'idle' | 'saving' | 'saved' | `error:${string}`>('idle');
  let currentDataJson = $state('');

  // Load initial data
  async function loadData(): Promise<Data> {
    const res = await fetch('/plugin-api/llm/settings');
    const data = await res.json();
    formData = data;
    currentDataJson = JSON.stringify(data);
    return data;
  }

  // The actual save function
  async function save() {
    if (!formData) return;
    status = 'saving';
    try {
      const res = await fetch('/plugin-api/llm/settings', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(formData)
      });
      if (!res.ok) throw new Error(await errorText(res));
      const newData = await res.json();
      currentDataJson = JSON.stringify(newData);
      status = 'saved';
      // Clear the "saved" checkmark after 2 seconds
      setTimeout(() => {
        if (status === 'saved') status = 'idle';
      }, 2000);
    } catch (err) {
      status = `error:${(err as Error).message || ''}`;
      formData = await loadData();
    }
  }

  // 1. Debounced Effect
  // Whenever formData changes, this effect runs.
  $effect(() => {
    // We access formData.model and formData.context_window_size
    // Svelte now "subscribes" to these specific values.
    if (formData) {
      const formJson = JSON.stringify(formData);
      if (formJson === currentDataJson) return; // No changes detected

      const timer = setTimeout(() => save(), 800);
      return () => clearTimeout(timer);
    }
  });

  let initPromise = loadData();

  // The admin only: the rules after the personality and lore, saved with a button
  type BasePrompt = { prompt: string; default: string };
  let basePrompt = $state<BasePrompt | null>(null);
  let basePromptDraft = $state('');
  let basePromptStatus = $state<'idle' | 'saving' | 'saved' | `error:${string}`>('idle');

  async function loadBasePrompt() {
    const res = await fetch('/plugin-api/llm/base-prompt');
    if (!res.ok) return;
    basePrompt = await res.json();
    basePromptDraft = basePrompt!.prompt;
  }

  /** `null` goes back to the default */
  async function saveBasePrompt(prompt: string | null) {
    basePromptStatus = 'saving';
    const res = await fetch('/plugin-api/llm/base-prompt', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ prompt })
    });
    if (!res.ok) {
      basePromptStatus = `error:${await errorText(res)}`;
      return;
    }
    basePrompt = await res.json();
    basePromptDraft = basePrompt!.prompt;
    basePromptStatus = 'saved';
    setTimeout(() => {
      if (basePromptStatus === 'saved') basePromptStatus = 'idle';
    }, 2000);
    // The full prompt below includes it
    loadData();
  }

  initPromise.then((data) => {
    if (data.allowed_to_edit) loadBasePrompt();
  });
</script>

{#await initPromise then _}
  {#if formData}
    <div class="settings-grid">
      <label for="model" class="font-medium whitespace-nowrap text-gray-400"> Model name </label>
      <input
        disabled={!formData.allowed_to_edit}
        id="model"
        type="text"
        bind:value={formData.model}
        placeholder="e.g. gpt-4"
        class="w-full rounded border border-gray-700 bg-gray-900 p-2 text-white transition-colors outline-none focus:border-indigo-500 disabled:cursor-not-allowed disabled:opacity-50"
      />

      <label for="window" class="font-medium whitespace-nowrap text-gray-400">
        Context window
      </label>
      <input
        disabled={!formData.allowed_to_edit}
        id="window"
        type="number"
        bind:value={formData.context_window_size}
        class="w-full rounded border border-gray-700 bg-gray-900 p-2 text-white transition-colors outline-none focus:border-indigo-500 disabled:cursor-not-allowed disabled:opacity-50"
      />

      <label for="image-enabled" class="font-medium whitespace-nowrap text-gray-400">
        Image generation
      </label>
      <label class="flex cursor-pointer items-center gap-2 text-sm text-gray-300">
        <input
          disabled={!formData.allowed_to_edit}
          id="image-enabled"
          type="checkbox"
          bind:checked={formData.image_generation_enabled}
          class="w-auto! disabled:cursor-not-allowed"
        />
        Let the bot make images when asked
      </label>

      <label for="image-model" class="font-medium whitespace-nowrap text-gray-400">
        Image model
      </label>
      <input
        disabled={!formData.allowed_to_edit}
        id="image-model"
        type="text"
        bind:value={formData.image_model}
        placeholder="e.g. google/gemini-3.1-flash-image"
        class="w-full rounded border border-gray-700 bg-gray-900 p-2 text-white transition-colors outline-none focus:border-indigo-500 disabled:cursor-not-allowed disabled:opacity-50"
      />

      <label for="images-per-day" class="font-medium whitespace-nowrap text-gray-400">
        Images per person per day
      </label>
      <input
        disabled={!formData.allowed_to_edit}
        id="images-per-day"
        type="number"
        min="0"
        bind:value={formData.images_per_user_per_day}
        class="w-full rounded border border-gray-700 bg-gray-900 p-2 text-white transition-colors outline-none focus:border-indigo-500 disabled:cursor-not-allowed disabled:opacity-50"
      />

      <!-- Channel window info (read only) -->
      <span class="text-xs font-medium whitespace-nowrap text-gray-400">
        Current window sizes
      </span>
      <div class="flex min-w-0 flex-col gap-2">
        {#if formData.context_windows_by_channel.length === 0}
          <span class="text-sm text-gray-400">No context windows available.</span>
        {:else}
          {#each formData.context_windows_by_channel as entry}
            <div class="flex min-w-0 items-center gap-2 text-left">
              <span class="truncate font-mono text-sm text-gray-300" title={entry.channel_name}
                >{entry.channel_name}</span
              >
              <span class="shrink-0 text-sm text-gray-400">- {entry.size}</span>
            </div>
          {/each}
        {/if}
      </div>

      <div class="status-row col-start-2 h-6 text-sm">
        {#if status === 'saving'}
          <span class="animate-pulse text-indigo-400">Saving changes...</span>
        {:else if status === 'saved'}
          <span class="text-green-500">✓ All changes saved</span>
        {:else if status.startsWith('error')}
          <span class="text-red-500">⚠ Error saving: {status.slice(6)}</span>
        {/if}
      </div>
    </div>

    {#if basePrompt}
      <section class="mt-6 flex flex-col gap-2 text-left">
        <label for="base-prompt" class="text-center! font-medium whitespace-nowrap text-gray-400">
          Base prompt <span class="text-xs font-normal text-gray-500">(admin only)</span>
        </label>
        <p class="text-xs text-gray-500">
          The rules after the personality and lore in every prompt. Applies from the next message.
          {#if basePrompt.prompt === basePrompt.default}
            This is the default from the code, and follows its updates.
          {:else}
            Customized: code updates to the default don't apply until it's reset.
          {/if}
        </p>
        <textarea
          id="base-prompt"
          bind:value={basePromptDraft}
          rows="16"
          class="w-full resize-y rounded border border-gray-700 bg-gray-900 p-2 font-mono text-sm text-white outline-none focus:border-indigo-500"
        ></textarea>
        <div class="flex items-center gap-2">
          <span class="text-sm">
            {#if basePromptStatus === 'saving'}
              <span class="animate-pulse text-indigo-400">Saving...</span>
            {:else if basePromptStatus === 'saved'}
              <span class="text-green-500">✓ Saved</span>
            {:else if basePromptStatus.startsWith('error')}
              <span class="text-red-500">⚠ {basePromptStatus.slice(6)}</span>
            {/if}
          </span>
          <button
            onclick={() => saveBasePrompt(null)}
            disabled={basePrompt.prompt === basePrompt.default}
            class="ml-auto cursor-pointer rounded px-3 py-1 text-sm text-gray-400 hover:bg-gray-800 hover:text-white disabled:cursor-not-allowed disabled:opacity-40"
          >
            Reset to default
          </button>
          <button
            onclick={() => saveBasePrompt(basePromptDraft)}
            disabled={basePromptDraft === basePrompt.prompt || !basePromptDraft.trim()}
            class="cursor-pointer rounded bg-indigo-600 px-3 py-1 text-sm font-medium text-white hover:bg-indigo-500 disabled:cursor-not-allowed disabled:opacity-40"
          >
            Save
          </button>
        </div>
      </section>
    {/if}

    <label for="prompt" class="text-center! font-medium whitespace-nowrap text-gray-400">
      Full prompt
    </label>
    <textarea
      id="prompt"
      readonly
      disabled
      value={formData.prompt}
      rows="10"
      class="mt-6 w-full resize-y rounded border border-gray-700 bg-gray-900 p-2 text-white transition-colors outline-none focus:border-indigo-500 disabled:cursor-not-allowed"
    ></textarea>
  {/if}
{:catch err}
  <div class="text-red-500">Error loading data: {err.message}</div>
{/await}

<style>
  @reference "../../../layout.css";

  .settings-grid {
    display: grid;
    width: 100%;
    grid-template-columns: 1fr; /* Single column stack */
    gap: 0.5rem;
  }

  label {
    text-align: left;
    margin-top: 1rem; /* Space above a new section */
  }

  input {
    width: 100%; /* Ensure it hits the edges */
    margin-bottom: 0.5rem;
  }

  /* Ensure the status row doesn't look for a 2nd column on mobile */
  .status-row {
    grid-column: 1;
    text-align: left;
  }

  @media (min-width: 640px) {
    .settings-grid {
      grid-template-columns: auto 1fr;
      gap: 1.5rem;
      align-items: center;
    }

    label {
      text-align: right;
      margin-top: 0;
    }

    input {
      margin-bottom: 0;
    }

    .status-row {
      grid-column: 2; /* Put it back under the input on desktop */
    }
  }
</style>
