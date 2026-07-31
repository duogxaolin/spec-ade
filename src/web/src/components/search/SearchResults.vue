<script setup lang="ts">
// Results grouped by file, one row per match (SPEC-006 §5.9, D37/D38).
//
// The grouping and the highlight slicing both live in pure modules
// (`search/group.ts`, `search/highlight.ts`) — this file renders what they return
// and turns a click into a `open` event carrying `path` + `line`, which the shell
// hands to the editor.
//
// Groups collapse locally: a 2000-match search over a monorepo is unreadable as a
// flat list, and collapsing is per-file state that no store needs to know about.

import { computed, ref } from 'vue';

import type { SearchFileError } from '../../api/search';
import { dirName, fileName, type FileGroup } from '../../search/group';
import { highlightMatch } from '../../search/highlight';

const props = defineProps<{
  groups: FileGroup[];
  errors?: SearchFileError[];
  running?: boolean;
  truncated?: boolean;
  matchCount?: number;
  fileCount?: number;
  /** True once a finished search produced nothing — distinct from "not started". */
  empty?: boolean;
}>();

const emit = defineEmits<{ open: [path: string, line: number] }>();

/** Paths the user collapsed. Absent = expanded, so new groups stream in open. */
const collapsed = ref(new Set<string>());

function toggle(path: string): void {
  const next = new Set(collapsed.value);
  if (next.has(path)) next.delete(path);
  else next.add(path);
  collapsed.value = next;
}

const showErrors = ref(false);
const errorCount = computed(() => props.errors?.length ?? 0);
</script>

<template>
  <div class="results">
    <p v-if="empty" class="results__note">Không tìm thấy kết quả nào.</p>

    <!-- The cap was hit: say so, rather than letting these read as every match. -->
    <p v-if="truncated" class="results__note results__note--warn">
      Đã đạt giới hạn kết quả — kết quả bị cắt bớt. Thu hẹp truy vấn hoặc dùng glob.
    </p>

    <div v-if="errorCount > 0" class="results__errors">
      <button type="button" class="results__errtoggle" @click="showErrors = !showErrors">
        {{ showErrors ? '▾' : '▸' }} {{ errorCount }} tệp không đọc được
      </button>
      <ul v-if="showErrors" class="results__errlist">
        <li v-for="err in errors" :key="err.path">
          <span class="results__errpath">{{ err.path }}</span>
          <span class="results__errdetail">{{ err.detail }}</span>
        </li>
      </ul>
    </div>

    <section v-for="group in groups" :key="group.path" class="results__group">
      <button type="button" class="results__head" @click="toggle(group.path)">
        <span class="results__caret">{{ collapsed.has(group.path) ? '▸' : '▾' }}</span>
        <span class="results__file">{{ fileName(group.path) }}</span>
        <span v-if="dirName(group.path)" class="results__dir">{{ dirName(group.path) }}</span>
        <span class="results__count">{{ group.matches.length }}</span>
      </button>

      <ul v-if="!collapsed.has(group.path)" class="results__list">
        <li v-for="match in group.matches" :key="`${match.line}:${match.text}`">
          <button
            type="button"
            class="results__row"
            :title="`${group.path}:${match.line}`"
            @click="emit('open', group.path, match.line)"
          >
            <span class="results__line">{{ match.line }}</span>
            <span class="results__text">
              <!-- Segments come pre-sliced in byte space; `mark` is the only styling. -->
              <template v-for="(seg, i) in highlightMatch(match)" :key="i">
                <mark v-if="seg.match" class="results__mark">{{ seg.text }}</mark>
                <template v-else>{{ seg.text }}</template>
              </template>
            </span>
          </button>
        </li>
      </ul>
    </section>

    <p v-if="running" class="results__note">Đang tìm…</p>
  </div>
</template>

<style scoped>
.results {
  display: flex;
  flex-direction: column;
  overflow: auto;
}
.results__note {
  margin: 0;
  padding: 6px;
  color: #7a7a7a;
  font-size: 12px;
}
.results__note--warn {
  color: #d3a83c;
}
.results__errors {
  padding: 2px 6px;
}
.results__errtoggle {
  border: 0;
  background: none;
  color: #e06c6c;
  font-size: 11px;
  cursor: pointer;
}
.results__errlist {
  margin: 2px 0 0;
  padding: 0 0 0 14px;
  list-style: none;
  font-size: 11px;
}
.results__errpath {
  color: #c4c4c4;
}
.results__errdetail {
  margin-left: 6px;
  color: #7a7a7a;
}
.results__head {
  display: flex;
  align-items: baseline;
  gap: 6px;
  width: 100%;
  padding: 2px 6px;
  border: 0;
  background: none;
  color: #c4c4c4;
  font-size: 12px;
  text-align: left;
  cursor: pointer;
}
.results__head:hover {
  background: #232323;
}
.results__caret {
  width: 10px;
  flex: none;
  color: #7a7a7a;
}
.results__file {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.results__dir {
  flex: 1;
  overflow: hidden;
  color: #7a7a7a;
  font-size: 11px;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.results__count {
  flex: none;
  color: #7a7a7a;
  font-size: 11px;
}
.results__list {
  margin: 0;
  padding: 0;
  list-style: none;
}
.results__row {
  display: flex;
  gap: 8px;
  width: 100%;
  padding: 1px 6px 1px 20px;
  border: 0;
  background: none;
  color: #9e9e9e;
  font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
  font-size: 12px;
  text-align: left;
  cursor: pointer;
}
.results__row:hover {
  background: #232323;
}
.results__line {
  flex: none;
  min-width: 36px;
  color: #5a5a5a;
  text-align: right;
}
.results__text {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: pre;
}
.results__mark {
  background: #4a3d18;
  color: #ffd479;
}
</style>
