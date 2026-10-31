<script lang="ts">
  type Reminder = {
    id: number;
    /** Unix seconds */
    due: number;
    text: string;
    user_name: string;
    channel_name: string;
    message_url: string;
    mine: boolean;
  };

  type Data = { reminders: Reminder[]; is_admin: boolean };

  let data = $state<Data | null>(null);
  let error = $state('');
  /** The reminder whose cancel button was clicked once, asking to confirm */
  let confirming = $state<number | null>(null);

  async function load() {
    try {
      const res = await fetch('/plugin-api/llm/reminders');
      if (!res.ok) throw new Error(await res.text());
      data = await res.json();
    } catch (err) {
      error = (err as Error).message;
    }
  }

  async function cancel(id: number) {
    if (confirming !== id) {
      confirming = id;
      return;
    }
    confirming = null;
    const res = await fetch('/plugin-api/llm/reminders/cancel', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ id })
    });
    // Also when it was just sent: reload either way
    if (!res.ok && res.status !== 404) error = await res.text();
    await load();
  }

  function time(unixSecs: number): string {
    return new Date(unixSecs * 1000).toLocaleString('en', {
      weekday: 'short',
      month: 'short',
      day: 'numeric',
      hour: '2-digit',
      minute: '2-digit'
    });
  }

  const relative = new Intl.RelativeTimeFormat('en', { numeric: 'auto' });

  function fromNow(unixSecs: number): string {
    const minutes = Math.round((unixSecs * 1000 - Date.now()) / 60_000);
    if (Math.abs(minutes) < 60) return relative.format(minutes, 'minute');
    const hours = Math.round(minutes / 60);
    if (Math.abs(hours) < 48) return relative.format(hours, 'hour');
    return relative.format(Math.round(hours / 24), 'day');
  }

  load();
</script>

<div class="flex w-full flex-col gap-4 text-left">
  <p class="text-xs text-gray-500">
    Ask the bot "remind me to … in 2 hours" and it replies to your message with a ping when it's
    due.
    {#if data?.is_admin}As the admin you also see everyone's, except those set in DMs.{/if}
  </p>

  {#if error}
    <div class="text-sm text-red-500">⚠ {error}</div>
  {/if}

  {#if data == null}
    {#if !error}<div class="text-sm text-gray-400">Loading reminders...</div>{/if}
  {:else if data.reminders.length === 0}
    <div class="text-sm text-gray-400">No pending reminders.</div>
  {:else}
    <div class="flex flex-col gap-2">
      {#each data.reminders as reminder (reminder.id)}
        <div class="flex flex-wrap items-start gap-x-4 gap-y-1 rounded-lg bg-gray-900 px-3 py-2">
          <div class="flex min-w-40 flex-col tabular-nums">
            <span class="text-sm text-gray-200">{time(reminder.due)}</span>
            <span class="text-xs text-gray-500">{fromNow(reminder.due)}</span>
          </div>
          <div class="flex min-w-0 flex-1 flex-col gap-1">
            <span class="text-sm break-words whitespace-pre-wrap text-gray-100"
              >{reminder.text}</span
            >
            <span class="text-xs text-gray-500">
              {#if !reminder.mine}{reminder.user_name || 'Someone'} ·{/if}
              {#if reminder.message_url}
                <a
                  href={reminder.message_url}
                  target="_blank"
                  rel="noreferrer"
                  class="hover:text-indigo-300">{reminder.channel_name}</a
                >
              {:else}
                {reminder.channel_name}
              {/if}
            </span>
          </div>
          <button
            onclick={() => cancel(reminder.id)}
            onblur={() => {
              if (confirming === reminder.id) confirming = null;
            }}
            class="cursor-pointer rounded px-2 py-1 text-xs transition-colors {confirming ===
            reminder.id
              ? 'bg-red-500/20 text-red-300'
              : 'text-gray-400 hover:text-white'}"
          >
            {confirming === reminder.id ? 'Sure?' : 'Cancel'}
          </button>
        </div>
      {/each}
    </div>
  {/if}
</div>
