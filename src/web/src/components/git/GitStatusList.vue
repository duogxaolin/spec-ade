<script setup lang="ts">
// The four status groups, with the actions each group allows (C46).
//
// Grouping and glyphs come from `git/status.ts` so they can be tested without a
// mount — this file is the rendering and the click targets, nothing else.
//
// Empty groups do not render at all, the same rule `ChatPlan` follows: a header
// reading "Xung đột 0" is noise on a clean tree, and the absence of the header is
// the information.

import { computed } from 'vue';

import type { StatusEntry } from '../../api/git';
import { groupEntries, nonEmptyGroups, type GroupId, type GroupedEntry } from '../../git/status';

const props = defineProps<{
  entries: StatusEntry[];
  /** Which row is open in the diff pane, as `group:path`. */
  selectedKey?: string | null;
  /** Disables every action while a mutation is in flight. */
  busy?: boolean;
}>();

const emit = defineEmits<{
  select: [row: GroupedEntry];
  stage: [paths: string[]];
  unstage: [paths: string[]];
  discard: [paths: string[]];
  resolve: [path: string];
}>();

const groups = computed(() => nonEmptyGroups(groupEntries(props.entries)));

/** Actions available on a row, decided by which group it is in. */
function rowActions(group: GroupId): { stage: boolean; unstage: boolean; discard: boolean; resolve: boolean } {
  return {
    // Staging an untracked file is `git add` on a new path; staging a changed one
    // is `git add` on a modified path. Both are the same call.
    stage: group === 'changed' || group === 'untracked',
    unstage: group === 'staged',
    // Discard is refused for untracked files by the server ([INVENTED-2]) — the
    // button is absent rather than present-and-failing.
    discard: group === 'changed',
    resolve: group === 'conflicted',
  };
}

/** "Stage all" for a group — one request instead of one per file. */
function pathsOf(rows: GroupedEntry[]): string[] {
  return rows.map((row) => row.path);
}
</script>

<template>
  <div class="status">
    <section v-for="group in groups" :key="group.id" class="status__group">
      <header class="status__head">
        <span class="status__title">{{ group.title }}</span>
        <span class="status__count">{{ group.entries.length }}</span>
        <span class="status__spacer" />
        <button
          v-if="group.id === 'changed' || group.id === 'untracked'"
          type="button"
          class="status__bulk"
          :disabled="busy"
          @click="emit('stage', pathsOf(group.entries))"
        >
          Stage tất cả
        </button>
        <button
          v-else-if="group.id === 'staged'"
          type="button"
          class="status__bulk"
          :disabled="busy"
          @click="emit('unstage', pathsOf(group.entries))"
        >
          Unstage tất cả
        </button>
      </header>

      <ul class="status__list">
        <li
          v-for="row in group.entries"
          :key="row.key"
          class="status__row"
          :class="{ 'status__row--active': row.key === selectedKey }"
        >
          <button
            type="button"
            class="status__open"
            :title="`${row.path} — ${row.label}`"
            @click="emit('select', row)"
          >
            <span class="status__glyph" :class="`status__glyph--${row.group}`" aria-hidden="true">
              {{ row.glyph }}
            </span>
            <span class="status__name">{{ row.name }}</span>
            <span v-if="row.dir" class="status__dir">{{ row.dir }}</span>
            <!-- A rename is two paths; showing only the new one hides what moved. -->
            <span v-if="row.origPath" class="status__orig">← {{ row.origPath }}</span>
          </button>

          <span class="status__actions">
            <button
              v-if="rowActions(row.group).stage"
              type="button"
              class="status__act"
              title="Stage"
              :disabled="busy"
              @click="emit('stage', [row.path])"
            >
              +
            </button>
            <button
              v-if="rowActions(row.group).unstage"
              type="button"
              class="status__act"
              title="Unstage"
              :disabled="busy"
              @click="emit('unstage', [row.path])"
            >
              −
            </button>
            <button
              v-if="rowActions(row.group).discard"
              type="button"
              class="status__act status__act--danger"
              title="Bỏ thay đổi (không hoàn tác được)"
              :disabled="busy"
              @click="emit('discard', [row.path])"
            >
              ⨯
            </button>
            <button
              v-if="rowActions(row.group).resolve"
              type="button"
              class="status__act"
              title="Mở trình giải xung đột"
              :disabled="busy"
              @click="emit('resolve', row.path)"
            >
              Giải
            </button>
          </span>
        </li>
      </ul>
    </section>

    <p v-if="groups.length === 0" class="status__clean">Không có thay đổi nào.</p>
  </div>
</template>

<style scoped>
.status {
  display: flex;
  flex-direction: column;
  gap: 8px;
}
.status__group {
  display: flex;
  flex-direction: column;
}
.status__head {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 2px 6px;
  color: #9e9e9e;
  font-size: 11px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
}
.status__count {
  color: #7a7a7a;
}
.status__spacer {
  flex: 1;
}
.status__bulk {
  border: 0;
  background: none;
  color: #7a9ec4;
  font-size: 11px;
  cursor: pointer;
}
.status__bulk:disabled {
  color: #5a5a5a;
  cursor: default;
}
.status__list {
  margin: 0;
  padding: 0;
  list-style: none;
}
.status__row {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 0 6px;
}
.status__row:hover,
.status__row--active {
  background: #232323;
}
.status__open {
  display: flex;
  flex: 1;
  align-items: baseline;
  gap: 6px;
  min-width: 0;
  padding: 2px 0;
  border: 0;
  background: none;
  color: #c4c4c4;
  font-size: 12px;
  text-align: left;
  cursor: pointer;
}
.status__glyph {
  width: 12px;
  flex: none;
  color: #7a7a7a;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  text-align: center;
}
.status__glyph--staged {
  color: #6fcf74;
}
.status__glyph--changed {
  color: #d3a83c;
}
.status__glyph--untracked {
  color: #7a9ec4;
}
.status__glyph--conflicted {
  color: #e06c6c;
}
.status__name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.status__dir,
.status__orig {
  overflow: hidden;
  color: #7a7a7a;
  font-size: 11px;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.status__actions {
  display: flex;
  flex: none;
  gap: 2px;
}
.status__act {
  min-width: 18px;
  padding: 0 4px;
  border: 0;
  background: none;
  color: #9e9e9e;
  font-size: 12px;
  cursor: pointer;
}
.status__act:hover {
  color: #e4e4e4;
}
.status__act--danger:hover {
  color: #e06c6c;
}
.status__act:disabled {
  color: #5a5a5a;
  cursor: default;
}
.status__clean {
  margin: 0;
  padding: 6px;
  color: #7a7a7a;
  font-size: 12px;
}
</style>
