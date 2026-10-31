<script lang="ts">
  import StackedBars from '../../../../lib/components/StackedBars.svelte';

  type Totals = {
    requests: number;
    images: number;
    prompt_tokens: number;
    cached_tokens: number;
    completion_tokens: number;
    reasoning_tokens: number;
    cost: number;
  };

  type Kind = 'chat' | 'command' | 'starboard' | 'summary' | 'caption';

  type Usage = {
    /** Keyed by UTC day number (days since the Unix epoch) */
    days: Record<string, Partial<Record<Kind, Totals>>>;
    recent: ({ at: number; kind: Kind; model: string } & Totals)[];
  };

  // Fixed order, validated on the dark surface (dataviz reference palette, dark steps)
  const KINDS: { id: Kind; name: string; color: string }[] = [
    { id: 'chat', name: 'Chat replies', color: '#3987e5' },
    { id: 'command', name: 'Commands', color: '#d95926' },
    { id: 'starboard', name: 'Starboard', color: '#199e70' },
    { id: 'summary', name: 'Summaries', color: '#c98500' },
    { id: 'caption', name: 'Image captions', color: '#d55181' }
  ];
  const HIT_RATE_COLOR = '#9085e9';
  const KIND_NAMES = Object.fromEntries(KINDS.map((k) => [k.id, k.name])) as Record<Kind, string>;

  const RANGES = [7, 30, 90] as const;
  let range = $state<(typeof RANGES)[number]>(30);

  const DAY_MS = 86_400_000;

  async function loadUsage(): Promise<Usage> {
    const res = await fetch('/plugin-api/llm/usage');
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  }

  let usagePromise = loadUsage();

  function emptyTotals(): Totals {
    return {
      requests: 0,
      images: 0,
      prompt_tokens: 0,
      cached_tokens: 0,
      completion_tokens: 0,
      reasoning_tokens: 0,
      cost: 0
    };
  }

  function add(into: Totals, other: Totals | undefined) {
    if (!other) return;
    for (const key of Object.keys(into) as (keyof Totals)[]) into[key] += other[key] ?? 0;
  }

  /** Every day of the range, oldest first, including days without requests */
  function daysOf(usage: Usage, count: number) {
    const today = Math.floor(Date.now() / DAY_MS);
    return Array.from({ length: count }, (_, i) => {
      const day = today - count + 1 + i;
      const kinds = usage.days[day] ?? {};
      const total = emptyTotals();
      for (const kind of KINDS) add(total, kinds[kind.id]);
      return { day, kinds, total };
    });
  }

  function summarize(usage: Usage, count: number) {
    const days = daysOf(usage, count);
    const total = emptyTotals();
    const byKind = KINDS.map((kind) => {
      const totals = emptyTotals();
      for (const d of days) add(totals, d.kinds[kind.id]);
      add(total, totals);
      return { kind, totals };
    });
    return { days, total, byKind };
  }

  function dayLabel(day: number): string {
    return new Date(day * DAY_MS).toLocaleDateString('en', {
      month: 'short',
      day: 'numeric',
      timeZone: 'UTC'
    });
  }

  function formatCost(cost: number): string {
    if (cost === 0) return '$0';
    if (cost < 0.01) return `$${cost.toFixed(4)}`;
    return `$${cost.toFixed(2)}`;
  }

  const compact = new Intl.NumberFormat('en', { notation: 'compact', maximumFractionDigits: 1 });
  const full = new Intl.NumberFormat('en');

  function hitRate(totals: Totals): number {
    return totals.prompt_tokens ? totals.cached_tokens / totals.prompt_tokens : 0;
  }

  function percent(ratio: number): string {
    return `${Math.round(ratio * 100)}%`;
  }

  function time(unixSecs: number): string {
    return new Date(unixSecs * 1000).toLocaleString('en', {
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    });
  }
</script>

{#snippet tile(label: string, value: string, hint: string)}
  <div class="rounded-lg bg-gray-900 p-4 text-left">
    <div class="text-xs text-gray-400">{label}</div>
    <div class="mt-1 text-2xl font-semibold text-gray-100 tabular-nums">{value}</div>
    <div class="mt-1 text-xs text-gray-500">{hint}</div>
  </div>
{/snippet}

{#await usagePromise}
  <div class="text-sm text-gray-400">Loading usage...</div>
{:then usage}
  {@const stats = summarize(usage, range)}
  <!-- Days since the first recorded request, so a short history isn't averaged over the range -->
  {@const firstUsed = stats.days.findIndex((d) => d.total.requests > 0)}
  {@const trackedDays = firstUsed < 0 ? 0 : stats.days.length - firstUsed}
  <div class="flex w-full flex-col gap-8">
    <!-- Range filter -->
    <div class="flex justify-end gap-1 text-sm">
      {#each RANGES as r}
        <button
          onclick={() => (range = r)}
          class="cursor-pointer rounded px-3 py-1 transition-colors {range === r
            ? 'bg-indigo-500/20 text-indigo-300'
            : 'text-gray-400 hover:text-white'}">{r} days</button
        >
      {/each}
    </div>

    <!-- KPI row -->
    <div class="grid grid-cols-2 gap-3 md:grid-cols-4">
      {@render tile('Cost', formatCost(stats.total.cost), `last ${range} days`)}
      {@render tile(
        'Per month',
        formatCost(trackedDays ? (stats.total.cost / trackedDays) * 30 : 0),
        'at this rate'
      )}
      {@render tile(
        'Requests',
        full.format(stats.total.requests),
        `${formatCost(stats.total.requests ? stats.total.cost / stats.total.requests : 0)} each on average`
      )}
      {@render tile(
        'Cache hit rate',
        percent(hitRate(stats.total)),
        `${compact.format(stats.total.cached_tokens)} of ${compact.format(stats.total.prompt_tokens)} input tokens`
      )}
    </div>

    <!-- Cost per day -->
    <section class="text-left">
      <h3 class="mb-3 text-sm font-medium text-gray-300">Cost per day</h3>
      <StackedBars
        series={KINDS.map((k) => ({ name: k.name, color: k.color }))}
        columns={stats.days.map((d) => ({
          label: dayLabel(d.day),
          values: KINDS.map((k) => d.kinds[k.id]?.cost ?? 0)
        }))}
        format={formatCost}
        tooltipFooter={(i) => `${full.format(stats.days[i].total.requests)} requests`}
      />
    </section>

    <!-- Cache hit rate per day -->
    <section class="text-left">
      <h3 class="mb-1 text-sm font-medium text-gray-300">Cache hit rate per day</h3>
      <p class="mb-3 text-xs text-gray-500">
        Share of input tokens read from the provider's cache, billed at a fraction of the price.
      </p>
      <StackedBars
        series={[{ name: 'Cache hit rate', color: HIT_RATE_COLOR }]}
        columns={stats.days.map((d) => ({ label: dayLabel(d.day), values: [hitRate(d.total)] }))}
        format={percent}
        max={1}
        height={160}
        tooltipFooter={(i) => {
          const t = stats.days[i].total;
          return t.prompt_tokens
            ? `${compact.format(t.cached_tokens)} of ${compact.format(t.prompt_tokens)} tokens`
            : 'No requests';
        }}
      />
    </section>

    <!-- Per kind -->
    <section class="text-left">
      <h3 class="mb-3 text-sm font-medium text-gray-300">By kind of request</h3>
      <div class="overflow-x-auto">
        <table class="w-full text-sm">
          <thead class="text-xs text-gray-400">
            <tr class="border-b border-gray-700">
              <th class="py-2 text-left font-medium">Kind</th>
              <th class="py-2 text-right font-medium">Requests</th>
              <th class="py-2 text-right font-medium">Images</th>
              <th class="py-2 text-right font-medium">Input tokens</th>
              <th class="py-2 text-right font-medium">Cached</th>
              <th class="py-2 text-right font-medium">Output tokens</th>
              <th class="py-2 text-right font-medium">Cost</th>
              <th class="py-2 text-right font-medium">Per request</th>
            </tr>
          </thead>
          <tbody class="text-gray-200 tabular-nums">
            {#each stats.byKind as { kind, totals }}
              <tr class="border-b border-gray-700/50">
                <td class="py-2">
                  <span class="flex items-center gap-2">
                    <span
                      class="inline-block h-2.5 w-2.5 rounded-sm"
                      style="background: {kind.color}"
                    ></span>
                    {kind.name}
                  </span>
                </td>
                <td class="py-2 text-right">{full.format(totals.requests)}</td>
                <td class="py-2 text-right">{full.format(totals.images)}</td>
                <td class="py-2 text-right">{compact.format(totals.prompt_tokens)}</td>
                <td class="py-2 text-right">{percent(hitRate(totals))}</td>
                <td class="py-2 text-right">{compact.format(totals.completion_tokens)}</td>
                <td class="py-2 text-right">{formatCost(totals.cost)}</td>
                <td class="py-2 text-right">
                  {formatCost(totals.requests ? totals.cost / totals.requests : 0)}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>

      <details class="mt-4 text-sm">
        <summary class="cursor-pointer text-gray-400 hover:text-white">Daily numbers</summary>
        <div class="mt-2 max-h-80 overflow-auto">
          <table class="w-full tabular-nums">
            <thead class="sticky top-0 bg-gray-800 text-xs text-gray-400">
              <tr class="border-b border-gray-700">
                <th class="py-2 text-left font-medium">Day</th>
                {#each KINDS as kind}
                  <th class="py-2 text-right font-medium">{kind.name}</th>
                {/each}
                <th class="py-2 text-right font-medium">Requests</th>
                <th class="py-2 text-right font-medium">Cache hits</th>
              </tr>
            </thead>
            <tbody class="text-gray-200">
              {#each [...stats.days].reverse() as d}
                <tr class="border-b border-gray-700/50">
                  <td class="py-1.5">{dayLabel(d.day)}</td>
                  {#each KINDS as kind}
                    <td class="py-1.5 text-right">{formatCost(d.kinds[kind.id]?.cost ?? 0)}</td>
                  {/each}
                  <td class="py-1.5 text-right">{full.format(d.total.requests)}</td>
                  <td class="py-1.5 text-right">{percent(hitRate(d.total))}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      </details>
    </section>

    <!-- Latest requests -->
    <section class="text-left">
      <h3 class="mb-3 text-sm font-medium text-gray-300">Latest requests</h3>
      {#if usage.recent.length === 0}
        <div class="text-sm text-gray-400">No requests recorded yet.</div>
      {:else}
        <div class="max-h-96 overflow-auto">
          <table class="w-full text-sm tabular-nums">
            <thead class="sticky top-0 bg-gray-800 text-xs text-gray-400">
              <tr class="border-b border-gray-700">
                <th class="py-2 text-left font-medium">Time</th>
                <th class="py-2 text-left font-medium">Kind</th>
                <th class="py-2 text-left font-medium">Model</th>
                <th class="py-2 text-right font-medium">Input</th>
                <th class="py-2 text-right font-medium">Cached</th>
                <th class="py-2 text-right font-medium">Output</th>
                <th class="py-2 text-right font-medium">Cost</th>
              </tr>
            </thead>
            <tbody class="text-gray-200">
              {#each [...usage.recent].reverse() as request}
                <tr class="border-b border-gray-700/50">
                  <td class="py-1.5 whitespace-nowrap text-gray-400">{time(request.at)}</td>
                  <td class="py-1.5 whitespace-nowrap">
                    {KIND_NAMES[request.kind]}
                    {#if request.images}
                      <span
                        class="ml-1 text-xs text-gray-400"
                        title="{request.images} image{request.images > 1 ? 's' : ''} sent"
                        >🖼 {request.images}</span
                      >
                    {/if}
                  </td>
                  <td class="max-w-48 truncate py-1.5 font-mono text-xs text-gray-400">
                    {request.model}
                  </td>
                  <td class="py-1.5 text-right">{full.format(request.prompt_tokens)}</td>
                  <td class="py-1.5 text-right">{percent(hitRate(request))}</td>
                  <td class="py-1.5 text-right">
                    {full.format(request.completion_tokens)}
                    {#if request.reasoning_tokens}
                      <span class="text-xs text-gray-500"
                        >({full.format(request.reasoning_tokens)} thinking)</span
                      >
                    {/if}
                  </td>
                  <td class="py-1.5 text-right">{formatCost(request.cost)}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {/if}
    </section>
  </div>
{:catch err}
  <div class="text-red-500">Error loading usage: {err.message}</div>
{/await}
