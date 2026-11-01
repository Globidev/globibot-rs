<script lang="ts">
  type Totals = {
    requests: number;
    prompt_tokens: number;
    cached_tokens: number;
    completion_tokens: number;
    cost: number;
  };

  type Outcome =
    | { type: 'answered'; rounds: number; totals: Totals }
    | { type: 'fell_back'; error: string };

  type Trace = {
    at: number;
    kind: 'chat' | 'direct_message';
    model: string;
    /** null in DMs */
    requester: string | null;
    message_url: string | null;
    calls: { name: string; arguments: string; result: string }[];
    duration_ms: number;
    outcome: Outcome;
  };

  /** A JSON schema, as much of it as the plugin uses */
  type Param = { type: string; description?: string; enum?: string[]; items?: Param };

  /** As sent to OpenRouter */
  type ToolDefinition = {
    function: {
      name: string;
      description: string;
      parameters: { properties: Record<string, Param>; required?: string[] };
    };
  };

  async function loadTraces(): Promise<{ traces: Trace[] }> {
    const res = await fetch('/plugin-api/llm/tool-traces');
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  }

  async function loadTools(): Promise<ToolDefinition[]> {
    const res = await fetch('/plugin-api/llm/tools');
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  }

  let tracesPromise = loadTraces();
  let toolsPromise = loadTools();

  function paramType(param: Param): string {
    const type = param.items ? `${paramType(param.items)}[]` : param.type;
    return param.enum ? `${type}: ${param.enum.join(' | ')}` : type;
  }

  let onlyWithCalls = $state(false);

  function summarize(traces: Trace[]) {
    const byTool: Record<string, { calls: number; answers: number }> = {};
    for (const trace of traces) {
      const names = trace.calls.map((call) => call.name);
      names.forEach((name, i) => {
        const stats = (byTool[name] ??= { calls: 0, answers: 0 });
        stats.calls += 1;
        // Once per answer
        if (names.indexOf(name) === i) stats.answers += 1;
      });
    }
    return {
      answers: traces.length,
      withCalls: traces.filter((t) => t.calls.length > 0).length,
      calls: traces.reduce((sum, t) => sum + t.calls.length, 0),
      fellBack: traces.filter((t) => t.outcome.type === 'fell_back').length,
      byTool: Object.entries(byTool).sort((a, b) => b[1].calls - a[1].calls)
    };
  }

  /** Pretty prints JSON arguments, or shows them as written if they aren't valid JSON */
  function pretty(json: string): string {
    try {
      return JSON.stringify(JSON.parse(json), null, 2);
    } catch {
      return json;
    }
  }

  function formatCost(cost: number): string {
    if (cost === 0) return '$0';
    if (cost < 0.01) return `$${cost.toFixed(4)}`;
    return `$${cost.toFixed(2)}`;
  }

  function percent(part: number, whole: number): string {
    return whole ? `${Math.round((part / whole) * 100)}%` : '0%';
  }

  function seconds(ms: number): string {
    return `${(ms / 1000).toFixed(1)}s`;
  }

  function time(unixSecs: number): string {
    return new Date(unixSecs * 1000).toLocaleString('en', {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    });
  }

  const full = new Intl.NumberFormat('en');
</script>

{#snippet tile(label: string, value: string, hint: string)}
  <div class="rounded-lg bg-gray-900 p-4 text-left">
    <div class="text-xs text-gray-400">{label}</div>
    <div class="mt-1 text-2xl font-semibold text-gray-100 tabular-nums">{value}</div>
    <div class="mt-1 text-xs text-gray-500">{hint}</div>
  </div>
{/snippet}

{#await tracesPromise}
  <div class="text-sm text-gray-400">Loading tool calls...</div>
{:then { traces }}
  {@const stats = summarize(traces)}
  {@const shown = [...traces].reverse().filter((t) => !onlyWithCalls || t.calls.length > 0)}
  <div class="flex w-full flex-col gap-8">
    <p class="text-left text-xs text-gray-500">
      Answers to pings and DMs from the last 3 days, where the model could call the tools below.
    </p>

    <!-- KPI row -->
    <div class="grid grid-cols-2 gap-3 md:grid-cols-4">
      {@render tile('Answers', full.format(stats.answers), 'last 3 days')}
      {@render tile(
        'Used tools',
        percent(stats.withCalls, stats.answers),
        `${full.format(stats.withCalls)} of ${full.format(stats.answers)} answers`
      )}
      {@render tile(
        'Tool calls',
        full.format(stats.calls),
        `${stats.withCalls ? (stats.calls / stats.withCalls).toFixed(1) : 0} per answer using tools`
      )}
      {@render tile(
        'Fell back',
        full.format(stats.fellBack),
        'answered without tools after an error'
      )}
    </div>

    <!-- Per tool -->
    <section class="text-left">
      <h3 class="mb-3 text-sm font-medium text-gray-300">By tool</h3>
      {#if stats.byTool.length === 0}
        <div class="text-sm text-gray-400">No tool calls yet.</div>
      {:else}
        <table class="w-full text-sm">
          <thead class="text-xs text-gray-400">
            <tr class="border-b border-gray-700">
              <th class="py-2 text-left font-medium">Tool</th>
              <th class="py-2 text-right font-medium">Calls</th>
              <th class="py-2 text-right font-medium">Answers using it</th>
            </tr>
          </thead>
          <tbody class="text-gray-200 tabular-nums">
            {#each stats.byTool as [name, toolStats]}
              <tr class="border-b border-gray-700/50">
                <td class="py-2 font-mono text-xs">{name}</td>
                <td class="py-2 text-right">{full.format(toolStats.calls)}</td>
                <td class="py-2 text-right">{full.format(toolStats.answers)}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>

    <!-- Available tools -->
    <section class="text-left">
      <h3 class="mb-3 text-sm font-medium text-gray-300">Available tools</h3>
      {#await toolsPromise}
        <div class="text-sm text-gray-400">Loading tools...</div>
      {:then tools}
        <div class="flex flex-col gap-2">
          {#each tools as { function: tool }}
            {@const params = Object.entries(tool.parameters.properties)}
            <details class="rounded-lg bg-gray-900 text-sm">
              <summary class="cursor-pointer px-3 py-2">
                <span class="font-mono text-xs text-indigo-300">{tool.name}</span>
                <span class="ml-2 text-xs text-gray-500">
                  {params.length} parameter{params.length === 1 ? '' : 's'}
                </span>
              </summary>
              <div class="flex flex-col gap-3 border-t border-gray-700/50 px-3 py-3">
                <p class="text-gray-300">{tool.description}</p>
                {#if params.length > 0}
                  <ul class="flex flex-col gap-1 text-xs">
                    {#each params as [name, param]}
                      <li>
                        <span class="font-mono text-gray-200">{name}</span>
                        <span class="font-mono text-gray-500">{paramType(param)}</span>
                        {#if !tool.parameters.required?.includes(name)}
                          <span class="text-gray-500">(optional)</span>
                        {/if}
                        {#if param.description}
                          <span class="text-gray-400">: {param.description}</span>
                        {/if}
                      </li>
                    {/each}
                  </ul>
                {/if}
              </div>
            </details>
          {/each}
        </div>
      {:catch err}
        <div class="text-red-500">Error loading tools: {err.message}</div>
      {/await}
    </section>

    <!-- Latest answers -->
    <section class="text-left">
      <div class="mb-3 flex items-center justify-between">
        <h3 class="text-sm font-medium text-gray-300">Latest answers</h3>
        <label class="flex cursor-pointer items-center gap-2 text-xs text-gray-400">
          <input type="checkbox" bind:checked={onlyWithCalls} />
          Only with tool calls
        </label>
      </div>
      {#if shown.length === 0}
        <div class="text-sm text-gray-400">Nothing recorded yet.</div>
      {:else}
        <div class="flex max-h-[40rem] flex-col gap-2 overflow-auto">
          {#each shown as trace}
            <details class="rounded-lg bg-gray-900 text-sm">
              <summary
                class="flex cursor-pointer flex-wrap items-center gap-x-3 gap-y-1 px-3 py-2 tabular-nums"
              >
                <span class="whitespace-nowrap text-gray-400">{time(trace.at)}</span>
                <span class="text-gray-200">
                  {#if trace.requester == null}
                    <span class="text-gray-400">DM</span>
                  {:else if trace.message_url}
                    <a
                      href={trace.message_url}
                      target="_blank"
                      rel="noreferrer"
                      class="hover:text-indigo-300"
                      onclick={(e) => e.stopPropagation()}>{trace.requester}</a
                    >
                  {:else}
                    {trace.requester}
                  {/if}
                </span>
                <span class="flex flex-wrap gap-1">
                  {#each trace.calls as call}
                    <span class="rounded bg-indigo-500/20 px-1.5 font-mono text-xs text-indigo-300"
                      >{call.name}</span
                    >
                  {:else}
                    <span class="text-xs text-gray-500">no tool calls</span>
                  {/each}
                </span>
                <span class="ml-auto flex items-center gap-3 text-xs text-gray-400">
                  {#if trace.outcome.type === 'answered'}
                    <span title="Requests to the model">
                      {trace.outcome.rounds} round{trace.outcome.rounds > 1 ? 's' : ''}
                    </span>
                    <span>{formatCost(trace.outcome.totals.cost)}</span>
                  {:else}
                    <span class="rounded bg-red-500/20 px-1.5 text-red-300">fell back</span>
                  {/if}
                  <span>{seconds(trace.duration_ms)}</span>
                </span>
              </summary>

              <div class="flex flex-col gap-3 border-t border-gray-700/50 px-3 py-3">
                <div class="font-mono text-xs text-gray-500">{trace.model}</div>
                {#if trace.outcome.type === 'fell_back'}
                  <pre
                    class="overflow-x-auto rounded bg-red-950/40 p-2 text-xs whitespace-pre-wrap text-red-300">{trace
                      .outcome.error}</pre>
                {/if}
                {#each trace.calls as call, i}
                  <div>
                    <div class="mb-1 text-xs text-gray-400">
                      {i + 1}. <span class="font-mono text-indigo-300">{call.name}</span>
                    </div>
                    <div class="grid gap-2 md:grid-cols-2">
                      <pre
                        class="overflow-x-auto rounded bg-gray-800 p-2 text-xs whitespace-pre-wrap text-gray-300">{pretty(
                          call.arguments
                        )}</pre>
                      <pre
                        class="overflow-x-auto rounded bg-gray-800 p-2 text-xs whitespace-pre-wrap text-gray-200">{call.result}</pre>
                    </div>
                  </div>
                {/each}
              </div>
            </details>
          {/each}
        </div>
      {/if}
    </section>
  </div>
{:catch err}
  <div class="text-red-500">Error loading tool calls: {err.message}</div>
{/await}
