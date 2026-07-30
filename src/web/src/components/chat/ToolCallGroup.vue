<script setup lang="ts">
// A run of adjacent tool calls as one row ([SPEC-004 INVENTED-1]).
//
// The summary line is the point: "✓ 3 tool calls" tells the user the batch
// succeeded without spending three rows on it. A batch containing a failure is not
// summarised away — it expands itself, because a hidden error is the one thing this
// collapsing must never cause.

import { computed, ref } from 'vue';

import type { ToolCallPayload } from '../../api/acp';
import { statusGlyph, toolStatus } from '../../chat/toolContent';
import ToolCallCard from './ToolCallCard.vue';

const props = defineProps<{
  toolCallIds: string[];
  /** The session's tool-call table, keyed by id. */
  calls: Record<string, ToolCallPayload>;
}>();

const emit = defineEmits<{
  (event: 'open-location', payload: { path: string; line: number | null }): void;
}>();

/** Ids whose call has arrived. A gap can leave an id with no payload. */
const present = computed(() => props.toolCallIds.filter((id) => props.calls[id]));
const single = computed(() => present.value.length <= 1);
const anyFailed = computed(() =>
  present.value.some((id) => toolStatus(props.calls[id]?.status).known === 'failed'),
);
const anyRunning = computed(() =>
  present.value.some((id) => {
    const known = toolStatus(props.calls[id]?.status).known;
    return known === 'in_progress' || known === 'pending';
  }),
);

const userExpanded = ref(false);
// A single call is its own card; several collapse unless something needs attention.
const expanded = computed(() => single.value || userExpanded.value || anyFailed.value);

/** Worst-first, so the glyph reports the batch honestly. */
const summaryGlyph = computed(() => {
  if (anyFailed.value) return statusGlyph('failed');
  if (anyRunning.value) return statusGlyph('in_progress');
  return statusGlyph('completed');
});

const summaryText = computed(() => {
  const n = present.value.length;
  if (anyRunning.value) return `${n} tool call đang chạy`;
  if (anyFailed.value) return `${n} tool call (có lỗi)`;
  return `${n} tool call`;
});
</script>

<template>
  <div class="tg">
    <button
      v-if="!single"
      class="tg__summary"
      :class="{ 'tg__summary--failed': anyFailed }"
      :aria-expanded="expanded"
      @click="userExpanded = !userExpanded"
    >
      <span class="tg__glyph" aria-hidden="true">{{ summaryGlyph }}</span>
      <span class="tg__text">{{ summaryText }}</span>
      <span class="tg__chevron" aria-hidden="true">{{ expanded ? '▾' : '▸' }}</span>
    </button>

    <div v-if="expanded" class="tg__list">
      <ToolCallCard
        v-for="id in present"
        :key="id"
        :call="calls[id]!"
        :default-open="single"
        @open-location="emit('open-location', $event)"
      />
    </div>
  </div>
</template>

<style scoped>
.tg {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.tg__summary {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 3px 8px;
  border: 1px solid #2c2c2c;
  border-radius: 4px;
  background: #1b1b1b;
  color: #b0b0b0;
  cursor: pointer;
  font: inherit;
  font-size: 11.5px;
  text-align: left;
}
.tg__summary--failed {
  border-color: #4a2c2c;
  color: #e0a5a5;
}
.tg__glyph {
  color: #6fcf74;
}
.tg__summary--failed .tg__glyph {
  color: #e57373;
}
.tg__text {
  flex: 1;
}
.tg__chevron {
  color: #7a7a7a;
}
.tg__list {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
</style>
