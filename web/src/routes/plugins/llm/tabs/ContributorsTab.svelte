<script lang="ts">
  const contributors = [
    {
      username: 'Huffa',
      avatarUrl: 'https://cdn.discordapp.com/avatars/81253500993937408/6d50d9284ec9d0ae2824ce954bf011c9.webp?size=160',
      contribution: '🎁 81.10€'
    },
    {
      username: 'Arcane',
      avatarUrl: 'https://cdn.discordapp.com/avatars/134152701884760064/a_e77b178af03e8054537852935e235626.gif?size=1024',
      contribution: '🎁 44.39€'
    },
    {
      username: 'Puddz',
      avatarUrl: 'https://cdn.discordapp.com/avatars/95891807514165248/a_20b1830b920e6b2c0d1f5f787a6db368.gif?size=1024',
      contribution: '🎁 11.01€'
    },
  ];
</script>

{#snippet contributorRow(username: string, avatarUrl: string) }
  <div class="flex items-center gap-3 max-w-3xs">
    <img src={avatarUrl} alt="{username}'s avatar" class="h-8 w-8 rounded-md" />
    <span class="text-sm font-semibold truncate">
      {username}
    </span>
  </div>
{/snippet}

<div class="flex flex-col items-center">
  <p class="mb-4 text-gray-300">
    Huge thanks to the following individuals for their contributions ❤️
  </p>
  <div class="grid grid-cols-2 items-center gap-4 max-w-sm">
    {#each contributors as contributor}
      {@render contributorRow(contributor.username, contributor.avatarUrl)}
      <span class="text-sm text-gray-300 font-mono text-left">
        {contributor.contribution}
      </span>
    {/each}
  </div>
  <p class="mt-4 text-xs text-gray-300">
    All monetary contributions go towards LLM API credit costs
  </p>
</div>
