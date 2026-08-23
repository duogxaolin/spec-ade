// The recursive renderer (SPEC-008 §5.1) — turns the immutable tree in the
// layout store into DOM. Splits become a flex row/column whose halves carry
// `flex: ratio 1 0%` / `flex: (1-ratio) 1 0%` (ratio belongs to `first`,
// [INVENTED-1]); leaves render PaneLeaf.
//
// Keys are structural identity, not array index: splits key by their path
// (stable under sibling re-ordering), leaves by their store-assigned `id`.
// That is what lets Acp/Terminal content survive tree restructuring — Vue
// moves the existing component instead of unmounting it (§5.8).

<script setup lang="ts">
import { computed, ref } from 'vue';

import type { PaneNode, PanePath, PaneSide } from '../../panes/tree';
import PaneLeafVue from './PaneLeaf.vue';
import PaneSplitterVue from './PaneSplitter.vue';

const props = defineProps<{
  node: PaneNode;
  /** Steps from the root to `node`; `[]` at the root. */
  path?: PanePath;
}>();

const path = computed<PanePath>(() => props.path ?? []);

// Template refs handed to the splitter for its two-phase drag math. They
// populate after mount; the splitter only reads them on user interaction,
// so the brief null window before mount is unreachable.
const containerEl = ref<HTMLElement | null>(null);
const firstEl = ref<HTMLElement | null>(null);
const secondEl = ref<HTMLElement | null>(null);

/** Flex-basis style for the first/second half of a split ([INVENTED-1]). */
function halfFlex(side: PaneSide, ratio: number): string {
  const r = side === 'first' ? ratio : 1 - ratio;
  return `${r} 1 0%`;
}
</script>

<template>
  <template v-if="node.kind === 'leaf'">
    <PaneLeafVue :leaf="node" :path="path" />
  </template>

  <div
    v-else
    ref="containerEl"
    class="pcontainer"
    :class="node.direction === 'horizontal' ? 'pcontainer--row' : 'pcontainer--col'"
  >
    <div ref="firstEl" class="pcontainer__half" :style="{ flex: halfFlex('first', node.ratio) }">
      <PaneContainer :node="node.first" :path="[...path, 'first']" />
    </div>

    <PaneSplitterVue
      v-if="firstEl && secondEl && containerEl"
      :key="`${path.join('/')}:splitter`"
      :path="path"
      :direction="node.direction"
      :first-el="firstEl"
      :second-el="secondEl"
      :container-el="containerEl"
    />
    <div
      v-else
      class="pcontainer__split-placeholder"
      :class="node.direction === 'horizontal' ? 'pcontainer--row-gap' : 'pcontainer--col-gap'"
    />

    <div ref="secondEl" class="pcontainer__half" :style="{ flex: halfFlex('second', node.ratio) }">
      <PaneContainer :node="node.second" :path="[...path, 'second']" />
    </div>
  </div>
</template>

<style scoped>
.pcontainer {
  display: flex;
  min-height: 0;
  min-width: 0;
  width: 100%;
  height: 100%;
}
.pcontainer--row {
  flex-direction: row;
}
.pcontainer--col {
  flex-direction: column;
}
.pcontainer__half {
  min-width: 0;
  min-height: 0;
  overflow: hidden;
  display: flex;
  flex-direction: column;
}
/* One-frame placeholder before template refs populate (pre-interaction only). */
.pcontainer--row-gap {
  width: var(--split-size, 5px);
}
.pcontainer--col-gap {
  height: var(--split-size, 5px);
}
</style>
