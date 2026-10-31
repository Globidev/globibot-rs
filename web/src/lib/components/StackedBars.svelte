<script lang="ts">
  // Stacked columns over a continuous range (e.g. days), with a per-column hover tooltip.
  // Colors come from the caller (CSS values), text stays in gray text tokens.

  type Series = { name: string; color: string };
  type Column = { label: string; values: number[] };

  let {
    series,
    columns,
    format,
    max = undefined,
    height = 200,
    tooltipFooter = undefined
  }: {
    series: Series[];
    columns: Column[];
    format: (value: number) => string;
    /** Fixed top of the scale, e.g. 1 for ratios. Defaults to a clean value above the data */
    max?: number;
    height?: number;
    tooltipFooter?: (index: number) => string | undefined;
  } = $props();

  const AXIS_WIDTH = 56;
  const LABELS_HEIGHT = 20;
  const TOP_PADDING = 8;
  const GAP = 2;
  const RADIUS = 4;
  const MAX_BAR = 24;

  let width = $state(600);
  let hovered = $state<number | null>(null);

  let plotWidth = $derived(Math.max(0, width - AXIS_WIDTH));
  let plotHeight = $derived(height - LABELS_HEIGHT - TOP_PADDING);
  let slot = $derived(columns.length ? plotWidth / columns.length : 0);
  let barWidth = $derived(Math.max(2, Math.min(MAX_BAR, slot * 0.7)));

  let totals = $derived(columns.map((c) => c.values.reduce((a, b) => a + b, 0)));
  let top = $derived(max ?? niceCeil(Math.max(0, ...totals)));
  let ticks = $derived([0, 0.25, 0.5, 0.75, 1].map((t) => t * top));
  // Show about 8 labels at most along the bottom
  let labelEvery = $derived(Math.max(1, Math.ceil(columns.length / 8)));

  function niceCeil(value: number): number {
    if (value <= 0) return 1;
    const magnitude = 10 ** Math.floor(Math.log10(value));
    const step = [1, 2, 2.5, 5, 10].find((s) => s * magnitude >= value) ?? 10;
    return step * magnitude;
  }

  function y(value: number): number {
    return TOP_PADDING + plotHeight - (value / top) * plotHeight;
  }

  // Segments of one column, bottom to top, with the 2px gap taken off each segment's top
  function segments(column: Column) {
    let base = 0;
    const parts: { index: number; y0: number; y1: number }[] = [];
    column.values.forEach((value, index) => {
      if (value <= 0) return;
      parts.push({ index, y0: y(base), y1: y(base + value) });
      base += value;
    });
    return parts.map((part, i) => ({
      ...part,
      isTop: i === parts.length - 1,
      y1: i === parts.length - 1 ? part.y1 : Math.min(part.y0, part.y1 + GAP)
    }));
  }

  // Rounded at the data end only, square at the baseline
  function topRoundedRect(x: number, yTop: number, w: number, h: number): string {
    const r = Math.min(RADIUS, h, w / 2);
    return `M${x},${yTop + h} V${yTop + r} Q${x},${yTop} ${x + r},${yTop} H${x + w - r} Q${x + w},${yTop} ${x + w},${yTop + r} V${yTop + h} Z`;
  }

  let tooltipLeft = $derived(
    hovered == null ? 0 : Math.min(AXIS_WIDTH + hovered * slot + slot / 2, width - 120)
  );
</script>

<div class="relative w-full" bind:clientWidth={width}>
  <svg {width} {height} role="img" aria-label="Chart, see the table below for the numbers">
    <!-- Gridlines and y ticks -->
    {#each ticks as tick}
      <line
        x1={AXIS_WIDTH}
        x2={width}
        y1={y(tick)}
        y2={y(tick)}
        class="stroke-gray-700"
        stroke-width="1"
        shape-rendering="crispEdges"
      />
      <text
        x={AXIS_WIDTH - 8}
        y={y(tick)}
        dy="0.32em"
        text-anchor="end"
        class="fill-gray-400 text-[11px]">{format(tick)}</text
      >
    {/each}

    {#each columns as column, i}
      {@const x = AXIS_WIDTH + i * slot + (slot - barWidth) / 2}
      <g opacity={hovered == null || hovered === i ? 1 : 0.45}>
        {#each segments(column) as part}
          {#if part.isTop}
            <path
              d={topRoundedRect(x, part.y1, barWidth, part.y0 - part.y1)}
              fill={series[part.index].color}
            />
          {:else}
            <rect
              {x}
              y={part.y1}
              width={barWidth}
              height={Math.max(0, part.y0 - part.y1)}
              fill={series[part.index].color}
            />
          {/if}
        {/each}
      </g>
      {#if i % labelEvery === 0}
        <text
          x={AXIS_WIDTH + i * slot + slot / 2}
          y={height - 4}
          text-anchor="middle"
          class="fill-gray-400 text-[11px]">{column.label}</text
        >
      {/if}
      <!-- Hit target: the whole column slot -->
      <rect
        x={AXIS_WIDTH + i * slot}
        y={TOP_PADDING}
        width={slot}
        height={plotHeight}
        fill="transparent"
        role="button"
        tabindex="0"
        aria-label={`${column.label}: ${format(totals[i])}`}
        onmouseenter={() => (hovered = i)}
        onmouseleave={() => (hovered = null)}
        onfocus={() => (hovered = i)}
        onblur={() => (hovered = null)}
        class="outline-none"
      />
    {/each}
  </svg>

  {#if hovered != null}
    {@const column = columns[hovered]}
    {@const footer = tooltipFooter?.(hovered)}
    <div
      class="pointer-events-none absolute top-0 z-10 min-w-36 -translate-x-1/2 rounded-md border border-gray-700 bg-gray-900 px-3 py-2 text-left text-xs shadow-lg"
      style="left: {Math.max(80, tooltipLeft)}px"
    >
      <div class="mb-1 font-semibold text-gray-200">{column.label}</div>
      {#each series as s, index}
        <div class="flex items-center justify-between gap-4">
          <span class="flex items-center gap-1.5 text-gray-400">
            <span class="inline-block h-2 w-2 rounded-sm" style="background: {s.color}"></span>
            {s.name}
          </span>
          <span class="text-gray-200 tabular-nums">{format(column.values[index])}</span>
        </div>
      {/each}
      {#if series.length > 1}
        <div
          class="mt-1 flex justify-between gap-4 border-t border-gray-700 pt-1 font-semibold text-gray-200"
        >
          <span>Total</span><span class="tabular-nums">{format(totals[hovered])}</span>
        </div>
      {/if}
      {#if footer}
        <div class="mt-1 text-gray-400">{footer}</div>
      {/if}
    </div>
  {/if}

  {#if series.length > 1}
    <div class="mt-2 flex flex-wrap justify-center gap-4 text-xs text-gray-300">
      {#each series as s}
        <span class="flex items-center gap-1.5">
          <span class="inline-block h-2.5 w-2.5 rounded-sm" style="background: {s.color}"></span>
          {s.name}
        </span>
      {/each}
    </div>
  {/if}
</div>
