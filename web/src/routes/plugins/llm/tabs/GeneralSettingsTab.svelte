<script lang="ts">
  type Data = {
    model: string;
    prompt: string;
    context_window_size: number;
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
      if (!res.ok) throw new Error(await res.text());
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

      <!-- Channel window info (read only) -->
      <span class="text-xs font-medium whitespace-nowrap text-gray-400">
        Current window sizes
      </span>
      <div class="flex flex-col gap-2">
        {#if formData.context_windows_by_channel.length === 0}
          <span class="text-sm text-gray-400">No context windows available.</span>
        {:else}
          {#each formData.context_windows_by_channel as entry}
            <div class="flex items-center gap-2">
              <span class="font-mono text-sm text-gray-300">#{entry.channel_name}</span>
              <span class="text-sm text-gray-400">- {entry.size}</span>
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
