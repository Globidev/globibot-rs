import type { PageLoad } from './$types';

import { authStore } from '../../lib/stores/auth.svelte';

export const load: PageLoad<void> = async ({ url }) => {
  const code = url.searchParams.get('code');
  if (code != null) {
    authStore.login(code);
    return;
  }
};
