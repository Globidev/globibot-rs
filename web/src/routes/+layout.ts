import type { LayoutLoad } from './$types';
import { pluginStore } from '../lib/stores/plugins';

export const ssr = false;

export const load: LayoutLoad<void> = async ({ fetch }) => {
  await pluginStore.fetchPlugins(fetch);
};
