<script setup lang="ts">
// Commit history with cursor paging (C11) and a click-through to one commit.
//
// "Load more" is a button rather than an infinite scroller: the cursor page is
// server-side ([SPEC-005 INVENTED-7]), and a scroll listener that fires mid-fetch
// would ask for the same cursor twice. A button that disables while busy cannot.
//
// A merge commit is marked because its diff is against the first parent only, so
// "why does this commit show no changes" has a visible answer.

import { absoluteTime, relativeTime } from '../../git/relativeTime';
import type { Commit } from '../../api/git';

defineProps<{
  commits: Commit[];
  /** `null` when the history ended — the button disappears rather than no-ops. */
  nextBefore: string | null;
  busy?: boolean;
  /** Highlighted row, i.e. the commit whose detail is open. */
  selectedOid?: string | null;
}>();

const emit = defineEmits<{
  select: [oid: string];
  loadMore: [];
}>();
</script>

<template>
  <div class="log">
    <p v-if="commits.length === 0" class="log__empty">Chưa có commit nào.</p>

    <ol v-else class="log__list">
      <li
        v-for="commit in commits"
        :key="commit.oid"
        class="log__item"
        :class="{ 'log__item--active': commit.oid === selectedOid }"
      >
        <button type="button" class="log__row" @click="emit('select', commit.oid)">
          <span class="log__short">{{ commit.short }}</span>
          <span class="log__summary">{{ commit.summary }}</span>
          <!-- A merge's diff is against its first parent only; saying so here
               explains a detail view that looks emptier than expected. -->
          <span v-if="commit.parents.length > 1" class="log__merge" title="Merge commit">⑃</span>
          <span class="log__author">{{ commit.author.name }}</span>
          <time class="log__time" :title="absoluteTime(commit.author.time)">
            {{ relativeTime(commit.author.time) }}
          </time>
        </button>
      </li>
    </ol>

    <button
      v-if="nextBefore"
      type="button"
      class="log__more"
      :disabled="busy"
      @click="emit('loadMore')"
    >
      {{ busy ? 'Đang tải…' : 'Tải thêm' }}
    </button>
  </div>
</template>

<style scoped>
.log {
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.log__empty {
  margin: 8px;
  color: #7a7a7a;
  font-size: 12px;
}
.log__list {
  flex: 1;
  margin: 0;
  padding: 0;
  overflow-y: auto;
  list-style: none;
}
.log__row {
  display: flex;
  gap: 6px;
  align-items: baseline;
  width: 100%;
  padding: 3px 6px;
  border: 0;
  background: none;
  color: #c4c4c4;
  font: inherit;
  font-size: 12px;
  text-align: left;
  cursor: pointer;
}
.log__row:hover {
  background: #232323;
}
.log__item--active .log__row {
  background: #24384a;
}
.log__short {
  color: #7fa8c9;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  flex: none;
}
.log__summary {
  flex: 1;
  overflow: hidden;
  white-space: nowrap;
  text-overflow: ellipsis;
}
.log__merge {
  flex: none;
  color: #b48ead;
}
.log__author,
.log__time {
  flex: none;
  color: #7a7a7a;
  font-size: 11px;
}
.log__more {
  margin: 4px 6px 6px;
  padding: 3px 8px;
  border: 1px solid #2c2c2c;
  border-radius: 3px;
  background: #1e1e1e;
  color: #c4c4c4;
  font-size: 12px;
  cursor: pointer;
}
.log__more:disabled {
  color: #6a6a6a;
  cursor: default;
}
</style>
