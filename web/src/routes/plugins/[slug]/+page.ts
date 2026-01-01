import { pluginStore } from '../../../lib/stores/plugins';
import type { PageLoad } from './$types';

export const load: PageLoad = async ({ params, parent }) => {
  await parent();
  return {
    plugin: null //$pluginStore.plugins find((p) => p.name === params.slug) ?? null
  };
};
