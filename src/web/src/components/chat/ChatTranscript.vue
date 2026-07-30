<script setup lang="ts">
// The transcript: rows, plan, and scroll behaviour (SPEC-004 §5.1, §5.6).
//
// Scroll rule: follow the bottom while the user is at the bottom, and stop the
// moment they scroll up. Unconditional auto-scroll is actively hostile — it yanks
// the view away from someone re-reading an earlier answer while the agent is still
// talking. The "at the bottom" test has a 32 px tolerance because subpixel layout
// means `scrollTop + clientHeight` rarely equals `scrollHeight` exactly.

import { nextTick, onMounted, ref, watch } from 'vue';

import type { SessionView } from '../../stores/acpSession';
import { groupEntries } from '../../chat/grouping';
import ChatPlan from './ChatPlan.vue';
import ChatThought from './ChatThought.vue';
import MarkdownBlock from './MarkdownBlock.vue';
import ToolCallGroup from './ToolCallGroup.vue';

const props = defineProps<{ view: SessionView }>();

const emit = defineEmits<{
  (event: 'open-location', payload: { path: string; line: number | null }): void;
}>();

const scroller = ref<HTMLElement | null>(null);
/** False once the user scrolls away from the bottom; true again when they return. */
const following = ref(true);

/** Distance from the bottom still counted as "at the bottom". */
const BOTTOM_SLACK_PX = 32;

const rows = ref(groupEntries(props.view.entries));

// `entries` is mutated in place by the fold (chunks append to the trailing block),
// so a deep watcher is required — watching the array identity would never fire.
watch(
  () => props.view.entries,
  (entries) => {
    rows.value = groupEntries(entries);
    void scrollToBottomIfFollowing();
  },
  { deep: true, immediate: true },
);

onMounted(() => {
  // A reattached session arrives with its whole transcript replayed; the user
  // expects to land at the newest message, not the oldest.
  void scrollToBottomIfFollowing();
});

function onScroll(): void {
  const el = scroller.value;
  if (!el) return;
  following.value = el.scrollHeight - el.scrollTop - el.clientHeight <= BOTTOM_SLACK_PX;
}

async function scrollToBottomIfFollowing(): Promise<void> {
  if (!following.value) return;
  // After the DOM updates: scrolling before the new row exists lands short.
  await nextTick();
  const el = scroller.value;
  if (el) el.scrollTop = el.scrollHeight;
}

function jumpToBottom(): void {
  following.value = true;
  void scrollToBottomIfFollowing();
}

/**
 * Whether a row is the one currently receiving chunks.
 *
 * Only the last row of a live turn streams, so the cursor cannot appear in two
 * places — and it disappears on `turn_complete` even if the last row is text.
 */
function isStreamingRow(index: number): boolean {
  return props.view.turnActive && index === rows.value.length - 1;
}
</script>

<template>
  <div class="ct">
    <div ref="scroller" class="ct__scroll" @scroll.passive="onScroll">
      <!-- A pruned log means this is not the whole conversation. Saying so beats
           letting a gap read as a complete exchange (SPEC-003 A13). -->
      <p v-if="view.hasGap" class="ct__gap">Một phần lịch sử đã bị server xoá.</p>

      <ChatPlan v-if="view.plan" :plan="view.plan" />

      <div class="ct__rows">
        <template v-for="(row, index) in rows" :key="row.key">
          <ToolCallGroup
            v-if="row.kind === 'toolGroup'"
            :tool-call-ids="row.toolCallIds"
            :calls="view.toolCalls"
            @open-location="emit('open-location', $event)"
          />

          <MarkdownBlock
            v-else-if="row.entry.kind === 'message'"
            class="ct__msg"
            :source="row.entry.text"
            :streaming="isStreamingRow(index)"
          />

          <ChatThought
            v-else-if="row.entry.kind === 'thought'"
            :text="row.entry.text"
            :streaming="isStreamingRow(index)"
          />

          <p v-else-if="row.entry.kind === 'turn_end'" class="ct__meta">
            {{ row.entry.label || '— hết lượt —' }}
          </p>

          <p v-else-if="row.entry.kind === 'gap'" class="ct__meta">
            — thiếu lịch sử trước seq {{ row.entry.fromSeq }} —
          </p>

          <p v-else-if="row.entry.kind === 'notice'" class="ct__meta">{{ row.entry.text }}</p>
        </template>
      </div>
    </div>

    <!-- Only while detached: the button exists to get back, so showing it at the
         bottom would be a control that does nothing. -->
    <button v-if="!following" class="ct__jump" @click="jumpToBottom">↓ Tin mới nhất</button>
  </div>
</template>

<style scoped>
.ct {
  position: relative;
  display: flex;
  flex: 1;
  min-height: 0;
  flex-direction: column;
}
.ct__scroll {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 10px 12px;
  /* Keeps the bottom edge from sitting flush against the composer. */
  scroll-padding-bottom: 8px;
}
.ct__rows {
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.ct__msg {
  font-size: 13.5px;
  line-height: 1.55;
}
.ct__meta {
  margin: 0;
  color: #7a7a7a;
  font-size: 11px;
}
.ct__gap {
  margin: 0 0 8px;
  color: #ffd79b;
  font-size: 12px;
}
.ct__jump {
  position: absolute;
  right: 14px;
  bottom: 10px;
  padding: 4px 10px;
  border: 1px solid #3a3a3a;
  border-radius: 12px;
  background: #262626;
  color: #d0d0d0;
  cursor: pointer;
  font: inherit;
  font-size: 11px;
}
</style>
