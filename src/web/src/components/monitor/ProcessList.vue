<script setup lang="ts">
// Top-N processes with sort, filter, and kill (SPEC-006 §5.9, D46).
//
// **Kill asks first (D46).** `SIGKILL` on the wrong row loses unsaved work in
// whatever it hit, and the row under the cursor moves every 3 s as the list
// re-sorts by CPU — a misclick is not hypothetical here. The confirmation names
// the process and pid so what is about to die is legible, and it is a two-step
// inline state rather than `window.confirm`, which is unstyleable and blocks the
// sample loop.
//
// The list is *not* re-sorted locally: the server picked the top N by the chosen
// key, so re-ordering here would show a ranking that does not match the selection.

import { ref } from 'vue';

import type { KillSignalName, ProcessInfo, SortBy } from '../../api/system';
import { formatBytes, formatPercent, formatUptime } from '../../monitor/sparkline';

defineProps<{
  processes: ProcessInfo[];
  sort: SortBy;
  filter: string;
  /** Total processes on the host, before the top-N cut. */
  total?: number;
  truncated?: boolean;
  busy?: boolean;
}>();

const emit = defineEmits<{
  kill: [pid: number, signal: KillSignalName];
  'update:sort': [sort: SortBy];
  'update:filter': [filter: string];
}>();

/** The pid awaiting confirmation, or null. Only one row can be armed at a time. */
const pendingKill = ref<number | null>(null);

function arm(pid: number): void {
  pendingKill.value = pid;
}

function cancel(): void {
  pendingKill.value = null;
}

function confirm(pid: number, signal: KillSignalName): void {
  pendingKill.value = null;
  emit('kill', pid, signal);
}
</script>

<template>
  <div class="plist">
    <div class="plist__bar">
      <input
        class="plist__filter"
        type="search"
        placeholder="Lọc tiến trình…"
        :value="filter"
        @input="emit('update:filter', ($event.target as HTMLInputElement).value)"
      />
      <button
        type="button"
        class="plist__sort"
        :class="{ 'plist__sort--on': sort === 'cpu' }"
        @click="emit('update:sort', 'cpu')"
      >
        CPU
      </button>
      <button
        type="button"
        class="plist__sort"
        :class="{ 'plist__sort--on': sort === 'memory' }"
        @click="emit('update:sort', 'memory')"
      >
        RAM
      </button>
    </div>

    <table class="plist__table">
      <thead>
        <tr>
          <th class="plist__th plist__th--pid">PID</th>
          <th class="plist__th">Tên</th>
          <th class="plist__th plist__th--num">CPU</th>
          <th class="plist__th plist__th--num">RAM</th>
          <th class="plist__th plist__th--num">Thời gian</th>
          <th class="plist__th plist__th--act" />
        </tr>
      </thead>
      <tbody>
        <tr v-for="p in processes" :key="p.pid" class="plist__row">
          <td class="plist__td plist__td--pid">{{ p.pid }}</td>
          <td class="plist__td plist__td--name" :title="p.cmd">
            {{ p.name }}
            <span class="plist__user">{{ p.user }}</span>
          </td>
          <td class="plist__td plist__td--num">{{ formatPercent(p.cpu) }}</td>
          <td class="plist__td plist__td--num">{{ formatBytes(p.memory) }}</td>
          <td class="plist__td plist__td--num">{{ formatUptime(p.runTimeSec) }}</td>
          <td class="plist__td plist__td--act">
            <template v-if="pendingKill === p.pid">
              <!-- D46: nothing has been sent yet; this row is the confirmation. -->
              <span class="plist__confirm">Kết thúc {{ p.name }} ({{ p.pid }})?</span>
              <button
                type="button"
                class="plist__act plist__act--danger"
                title="SIGTERM"
                :disabled="busy"
                @click="confirm(p.pid, 'term')"
              >
                Term
              </button>
              <button
                type="button"
                class="plist__act plist__act--danger"
                title="SIGKILL — không cho tiến trình dọn dẹp"
                :disabled="busy"
                @click="confirm(p.pid, 'kill')"
              >
                Kill
              </button>
              <button type="button" class="plist__act" @click="cancel">Huỷ</button>
            </template>
            <button
              v-else
              type="button"
              class="plist__act"
              title="Kết thúc tiến trình"
              :disabled="busy"
              @click="arm(p.pid)"
            >
              ⨯
            </button>
          </td>
        </tr>
      </tbody>
    </table>

    <p v-if="processes.length === 0" class="plist__note">Không có tiến trình nào khớp.</p>
    <p v-else-if="truncated" class="plist__note">
      Hiển thị {{ processes.length }} / {{ total }} tiến trình.
    </p>
  </div>
</template>

<style scoped>
.plist {
  display: flex;
  flex-direction: column;
  min-height: 0;
  gap: 4px;
}
.plist__bar {
  display: flex;
  gap: 4px;
}
.plist__filter {
  flex: 1;
  min-width: 0;
  padding: 2px 6px;
  border: 1px solid #333;
  border-radius: 3px;
  background: #1c1c1c;
  color: #e4e4e4;
  font-size: 12px;
}
.plist__sort {
  border: 1px solid transparent;
  border-radius: 3px;
  background: none;
  color: #7a7a7a;
  font-size: 11px;
  cursor: pointer;
}
.plist__sort--on {
  border-color: #4a6d8c;
  background: #24313c;
  color: #9ec4e4;
}
.plist__table {
  width: 100%;
  border-collapse: collapse;
  font-size: 12px;
}
.plist__th {
  padding: 2px 6px;
  color: #7a7a7a;
  font-size: 11px;
  font-weight: 400;
  text-align: left;
  text-transform: uppercase;
}
.plist__th--num,
.plist__td--num {
  text-align: right;
  font-variant-numeric: tabular-nums;
}
.plist__th--pid,
.plist__td--pid {
  width: 60px;
  color: #7a7a7a;
  font-variant-numeric: tabular-nums;
}
.plist__row:hover {
  background: #232323;
}
.plist__td {
  padding: 1px 6px;
  color: #c4c4c4;
  white-space: nowrap;
}
.plist__td--name {
  overflow: hidden;
  max-width: 0;
  text-overflow: ellipsis;
}
.plist__user {
  margin-left: 6px;
  color: #5a5a5a;
  font-size: 11px;
}
.plist__td--act {
  text-align: right;
}
.plist__confirm {
  margin-right: 6px;
  color: #d3a83c;
  font-size: 11px;
}
.plist__act {
  padding: 0 4px;
  border: 0;
  background: none;
  color: #9e9e9e;
  font-size: 11px;
  cursor: pointer;
}
.plist__act:hover {
  color: #e4e4e4;
}
.plist__act--danger {
  color: #e06c6c;
}
.plist__act:disabled {
  color: #5a5a5a;
  cursor: default;
}
.plist__note {
  margin: 0;
  padding: 4px 6px;
  color: #7a7a7a;
  font-size: 11px;
}
</style>
