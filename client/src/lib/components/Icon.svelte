<script lang="ts">
  import { ICON, type IconName } from "../presentation/icons";

  let { name }: { name: IconName } = $props();
</script>

{@html ICON[name]}
