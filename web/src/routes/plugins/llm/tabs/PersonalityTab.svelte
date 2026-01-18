<script lang="ts">
  type Data = {
    personality: string;
    available_personalities: string[];
    prompt: string;
  };

  let formData = $state<Data | null>(null);
  let status = $state<'idle' | 'saving' | 'saved' | `error:${string}`>('idle');

  // Load initial data
  async function loadData(): Promise<Data> {
    const res = await fetch('/plugin-api/llm/personality');
    const data = await res.json();
    formData = data;
    return data;
  }

  // The actual save function
  async function save() {
    if (!formData) return;
    status = 'saving';
    try {
      const res = await fetch('/plugin-api/llm/personality', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(formData)
      });
      if (!res.ok) throw new Error(await res.text());
      const newData: Data = await res.json();
      formData = newData;
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

  let initPromise = loadData();
</script>

{#await initPromise then _}
  {#if formData}
    <div class="settings-grid">
      <label for="personality" class="font-medium whitespace-nowrap text-gray-400">
        Personality
      </label>
      <select
        id="personality"
        bind:value={formData.personality}
        onchange={() => save()}
        class="w-full rounded border border-gray-700 bg-gray-900 p-2 text-white transition-colors outline-none focus:border-indigo-500"
      >
        {#each formData.available_personalities as personality}
          <option value={personality}>{personality}</option>
        {/each}
      </select>

      <span class="font-medium whitespace-nowrap text-gray-400"> Personality prompt </span>
      <div class="flex flex-col gap-2">
        <textarea
          readonly
          disabled
          bind:value={formData.prompt}
          rows="20"
          class="w-full resize-y rounded border border-gray-700 bg-gray-900 p-2 text-white transition-colors outline-none focus:border-indigo-500 disabled:cursor-not-allowed"
        ></textarea>
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

  select {
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

    select {
      margin-bottom: 0;
    }
  }
</style>
