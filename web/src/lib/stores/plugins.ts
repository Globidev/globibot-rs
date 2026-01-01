import { writable } from 'svelte/store';
import type { Plugin } from '../types';

class PluginStore {
  plugins = writable<Plugin[]>([]);

  async fetchPlugins(fetch: typeof window.fetch) {
    const res = await fetch('/api/plugins');
    const plugins: Plugin[] = await res.json();
    this.plugins.set(plugins);
  }

  #handleEvent(event: MessageEvent) {
    const data: ServerEvent = JSON.parse(event.data);
    if ('RemovedPlugin' in data) {
      const pluginName = data.RemovedPlugin;
      this.plugins.update((plugins) => plugins.filter((p) => p.name !== pluginName));
    } else if ('UpsertedPlugin' in data) {
      const upsertedPlugin = data.UpsertedPlugin;
      this.plugins.update((plugins) => {
        const index = plugins.findIndex((p) => p.name === upsertedPlugin.name);
        if (index !== -1) {
          plugins[index] = upsertedPlugin;
        } else {
          plugins.push(upsertedPlugin);
        }
        return plugins;
      });
    }
  }

  #eventSource: EventSource | null = null;
  listenForUpdates() {
    const eventSource = new EventSource('/api/sse');

    eventSource.onmessage = (event) => this.#handleEvent(event);
    eventSource.onerror = (error) => console.log('👀', 'SSE ERROR', error);

    this.#eventSource = eventSource;
  }

  stopListeningForUpdates() {
    if (this.#eventSource) {
      this.#eventSource.close();
      this.#eventSource = null;
    }
  }
}

type ServerEvent = { UpsertedPlugin: Plugin } | { RemovedPlugin: string };

export const pluginStore = new PluginStore();
