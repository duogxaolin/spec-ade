<script setup lang="ts">
// Agent reasoning, collapsed by default ([SPEC-004 INVENTED-2]).
//
// Reasoning is routinely longer than the answer, so showing it expanded pushes the
// reply off screen — the user reads the thinking and never sees the conclusion. It
// stays available because when an agent gets something wrong, the reasoning is where
// the reason is.
//
// Kept as its own component rather than a flag on the message bubble: SPEC-003's
// fold already distinguishes thought from message, and collapsing logic on a shared
// component would make one prop change both.

import { computed, ref } from 'vue';

import MarkdownBlock from './MarkdownBlock.vue';

const props = defineProps<{
  text: string;
  /** Chunks still arriving for this block. */
  streaming?: boolean;
}>();

const open = ref(false);

/** A one-line peek so a closed block still says what it is about. */
const preview = computed(() => {
  const firstLine = props.text.trim().split('\n')[0] ?? '';
  return firstLine.length > 80 ? `${firstLine.slice(0, 80)}…` : firstLine;
});
</script>

<template>
  <div class="th">
    <button class="th__toggle" :aria-expanded="open" @click="open = !open">
      <span class="th__chevron" aria-hidden="true">{{ open ? '▾' : '▸' }}</span>
      <span class="th__label">{{ streaming ? 'Đang suy nghĩ…' : 'Suy nghĩ' }}</span>
      <span v-if="!open && preview" class="th__preview">{{ preview }}</span>
    </button>

    <!-- Rendered as markdown too: reasoning contains code and lists as often as
         the reply does, and showing it raw would make it the harder half to read. -->
    <div v-if="open" class="th__body">
      <MarkdownBlock :source="text" :streaming="streaming" />
    </div>
  </div>
</template>

<style scoped>
.th {
  border-left: 2px solid #3a3a3a;
  padding-left: 8px;
}
.th__toggle {
  display: flex;
  align-items: baseline;
  gap: 6px;
  width: 100%;
  padding: 2px 0;
  border: 0;
  background: none;
  color: #8a8a8a;
  cursor: pointer;
  font: inherit;
  font-size: 11.5px;
  text-align: left;
}
.th__label {
  font-style: italic;
  white-space: nowrap;
}
.th__preview {
  min-width: 0;
  overflow: hidden;
  color: #6e6e6e;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.th__body {
  padding: 4px 0 2px;
  color: #9e9e9e;
  font-size: 12.5px;
}
</style>
