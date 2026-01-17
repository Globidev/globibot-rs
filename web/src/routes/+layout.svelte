<script lang="ts">
  import './layout.css';
  import favicon from '$lib/assets/favicon.png';
  import globibotLogo from '$lib/assets/globibot.webp';
  import NavbarPlugins from '$lib/components/NavbarPlugins.svelte';
  import { pluginStore } from '../lib/stores/plugins';
  import { authStore } from '../lib/stores/auth.svelte';
  import { onDestroy, onMount } from 'svelte';
  import DiscordLogin from '../lib/components/DiscordLogin.svelte';

  const { children } = $props();

  onMount(() => {
    pluginStore.listenForUpdates();
    authStore.loadUserProfile();
  });
  onDestroy(() => {
    pluginStore.stopListeningForUpdates();
  });
</script>

<svelte:head><link rel="icon" href={favicon} /></svelte:head>

<nav class="border-border bg-background border-b">
  <div
    class="mx-auto
    flex
    w-full
    max-w-5xl
    items-center
    justify-between
    px-6
    py-4
    lg:max-w-6xl
    xl:max-w-7xl
    2xl:max-w-352"
  >
    <a href="/" class="text-lg font-semibold">
      <img src={globibotLogo} alt="Globibot" class="mr-2 inline-block h-6 w-6 rounded-lg" />
      <span>Globibot</span>
    </a>

    <div class="flex items-center gap-6 text-sm">
      <NavbarPlugins />

      <a
        href="https://github.com/Globidev/globibot-rs"
        target="_blank"
        class="icon-[mdi--github] text-xl text-gray-600"
      >
        GitHub
      </a>

      <DiscordLogin />
    </div>
  </div>
</nav>

<div class="flex min-h-screen flex-col bg-[#1A1A1E] text-white">
  <main
    class="
      mx-auto
      w-full
      max-w-5xl
      px-6
      py-12
      lg:max-w-6xl
      xl:max-w-7xl
      2xl:max-w-352
    "
  >
    {@render children()}
  </main>
</div>
