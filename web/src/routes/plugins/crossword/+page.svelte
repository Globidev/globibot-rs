<script lang="ts">
  import DiscordLogin from '../../../lib/components/DiscordLogin.svelte';
  import { authStore } from '../../../lib/stores/auth.svelte';

  type Player = {
    user_id: string;
    username: string;
    avatar_url: string | null;
  };

  type Leaderboard = {
    latest_date: string | null;
    latest: { player: Player; solve_time_secs: number }[];
    all_time: {
      player: Player;
      solved: number;
      wins: number;
      best_secs: number;
      average_secs: number;
      current_streak: number;
    }[];
  };

  const tabs = [
    { id: 'latest', label: '🧩 Latest puzzle' },
    { id: 'all_time', label: '🏆 All time' }
  ] as const;

  type TabId = (typeof tabs)[number]['id'];

  let storedId = window.location.hash.slice(1);
  let activeTabId = $state<TabId>(storedId === 'all_time' ? 'all_time' : 'latest');

  async function loadLeaderboard(): Promise<Leaderboard> {
    const res = await fetch('/plugin-api/crossword/leaderboard');
    if (!res.ok) throw new Error(await res.text());
    return res.json();
  }

  let leaderboardPromise = $derived(authStore.user ? loadLeaderboard() : null);

  function formatTime(secs: number): string {
    const minutes = Math.floor(secs / 60);
    const seconds = secs % 60;
    return `${minutes}:${seconds.toString().padStart(2, '0')}`;
  }

  function medal(rank: number): string {
    return ['🥇', '🥈', '🥉'][rank] ?? `${rank + 1}.`;
  }
</script>

{#snippet playerInfo(player: Player)}
  <div class="flex max-w-3xs items-center gap-3">
    {#if player.avatar_url}
      <img src={player.avatar_url} alt="" class="h-8 w-8 rounded-md" />
    {:else}
      <div class="h-8 w-8 rounded-md bg-gray-700"></div>
    {/if}
    <span class="truncate text-sm font-semibold">{player.username}</span>
  </div>
{/snippet}

<div class="mx-auto max-w-5xl px-6">
  <header class="mb-12 text-center">
    <h1 class="text-2xl font-semibold tracking-tight">Crossword plugin</h1>
    <p class="mt-2 text-sm text-gray-400">Play the mini crossword in Discord with /crossword</p>
  </header>

  <div class="flex flex-col items-center gap-6 text-center">
    {#if !authStore.initialized}
      <div class="text-sm">Loading authentication status...</div>
    {:else if authStore.user == null}
      <div class="text-sm">You must be logged in to see the crossword leaderboard.</div>
      <DiscordLogin />
    {:else}
      <div class="mb-6 flex border-b border-gray-700">
        {#each tabs as tab}
          <button
            onclick={() => {
              activeTabId = tab.id;
              location.hash = tab.id;
            }}
            class="-mb-px cursor-pointer px-4 py-2 transition-colors {activeTabId === tab.id
              ? 'border-b-2 border-indigo-500 text-indigo-400'
              : 'text-gray-400 hover:text-white'}"
          >
            {tab.label}
          </button>
        {/each}
      </div>

      <div class="flex w-full flex-1 flex-col rounded-lg bg-gray-800 p-4">
        {#await leaderboardPromise}
          <div class="animate-pulse text-gray-400">Counting the squares...</div>
        {:then leaderboard}
          {#if !leaderboard || leaderboard.latest_date == null}
            <div class="text-sm text-gray-400">Nobody has solved a crossword yet.</div>
          {:else if activeTabId === 'latest'}
            <p class="mb-4 text-sm text-gray-300">Puzzle of {leaderboard.latest_date}</p>
            <div class="grid grid-cols-[auto_1fr_auto] items-center gap-x-4 gap-y-2 text-left">
              {#each leaderboard.latest as solve, rank}
                <span class="w-8 text-center">{medal(rank)}</span>
                {@render playerInfo(solve.player)}
                <span class="font-mono text-sm text-gray-300">
                  {formatTime(solve.solve_time_secs)}
                </span>
              {/each}
            </div>
          {:else}
            <div class="overflow-x-auto">
              <table class="w-full text-left text-sm">
                <thead class="text-xs text-gray-400 uppercase">
                  <tr>
                    <th class="px-2 py-2"></th>
                    <th class="px-2 py-2">Player</th>
                    <th class="px-2 py-2 text-right">Solved</th>
                    <th class="px-2 py-2 text-right" title="Puzzles solved the fastest">Wins</th>
                    <th class="px-2 py-2 text-right">Best</th>
                    <th class="px-2 py-2 text-right">Average</th>
                    <th class="px-2 py-2 text-right">Streak</th>
                  </tr>
                </thead>
                <tbody>
                  {#each leaderboard.all_time as stats, rank}
                    <tr class="border-t border-gray-700">
                      <td class="px-2 py-2 text-center">{medal(rank)}</td>
                      <td class="px-2 py-2">{@render playerInfo(stats.player)}</td>
                      <td class="px-2 py-2 text-right font-mono">{stats.solved}</td>
                      <td class="px-2 py-2 text-right font-mono">{stats.wins}</td>
                      <td class="px-2 py-2 text-right font-mono">{formatTime(stats.best_secs)}</td>
                      <td class="px-2 py-2 text-right font-mono">
                        {formatTime(stats.average_secs)}
                      </td>
                      <td class="px-2 py-2 text-right font-mono">
                        {stats.current_streak > 0 ? `🔥 ${stats.current_streak}` : '0'}
                      </td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            </div>
          {/if}
        {:catch err}
          <div class="text-sm text-red-400">Failed to load the leaderboard: {err.message}</div>
        {/await}
      </div>
    {/if}
  </div>
</div>
