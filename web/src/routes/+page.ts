import type { PageLoad } from './$types';

export const load: PageLoad<void> = async () => {
  await new Promise((resolve) => setTimeout(resolve, 1000));
};
