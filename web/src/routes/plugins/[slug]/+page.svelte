<script lang="ts">
  import type { PageProps } from './$types';

  let { data }: PageProps = $props();
</script>

<span>{data.plugin?.name ?? '???'}</span>
