<script lang="ts">
  import { clickOutside } from '../actions/clickOutside';
  import { pluginStore } from '../stores/plugins';

  let open = $state(false);
  const { plugins } = pluginStore;
</script>

<div class="relative" use:clickOutside={() => (open = false)}>
  <button class="flex cursor-pointer items-center gap-1" onclick={() => (open = !open)}>
    Plugins
    <span class="icon-[mdi--chevron-down] text-xs"></span>
  </button>

  {#if open}
    <div
      class="absolute right-0 mt-2 w-48 rounded-lg border border-gray-200 bg-white shadow-lg dark:border-gray-700 dark:bg-gray-900"
    >
      <ul class="py-1 text-sm text-white">
        {#each $plugins as plugin}
          <li>
            <a
              href={`/plugins/${plugin.name}`}
              class="block px-4 py-2 hover:bg-gray-100 dark:hover:bg-gray-800"
              onclick={() => (open = false)}
            >
              {plugin.name}
            </a>
          </li>
        {/each}
      </ul>
    </div>
  {/if}
</div>
