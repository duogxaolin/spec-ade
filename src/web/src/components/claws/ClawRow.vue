<script setup lang="ts">
// One claw row: name, agent, state badge, next run, and the Start/Stop/Delete
// actions (SPEC-007 §5.8).
//
// Props-in/events-out, same rule as the git sub-components: this component knows
// a button was clicked, the panel decides what that means. State colors come
// from `stateClass` — `error` must be loud because a claw that gave up silently
// is exactly how schedules stop firing without anyone noticing.

import { computed } from 'vue';

import type { ClawRow } from '../../api/claws';

const props = defineProps<{ claw: ClawRow; busy?: boolean }>();

const emit = defineEmits<{
  start: [];
  stop: [];
  remove: [];
  edit: [];
}>();

const running = computed(() => props.claw.status.state !== 'stopped' && props.claw.status.state !== 'error');

const stateLabel = computed(() => {
  switch (props.claw.status.state) {
    case 'running':
      return 'đang chạy';
    case 'idle':
      return 'chờ lịch';
    case 'starting':
      return 'đang khởi động';
    case 'error':
      return 'lỗi';
    default:
      return 'dừng';
  }
});

function stateClass(state: string): string {
  return `claw__state--${state}`;
}

/** Local time rendering; the server sends RFC3339 ([INVENTED-11]). */
const nextRun = computed(() => {
  const raw = props.claw.status.nextRunAt;
  if (!raw) return '';
  const date = new Date(raw);
  return Number.isNaN(date.getTime()) ? '' : date.toLocaleTimeString();
});

const lastRun = computed(() => {
  const raw = props.claw.status.lastRunAt;
  if (!raw) return '';
  const date = new Date(raw);
  return Number.isNaN(date.getTime()) ? '' : date.toLocaleString();
});
</script>

<template>
  <div class="claw" :class="{ 'claw--error': claw.status.state === 'error' }">
    <div class="claw__main">
      <div class="claw__title">
        <strong>{{ claw.name }}</strong>
        <span class="claw__state" :class="stateClass(claw.status.state)">{{ stateLabel }}</span>
        <span v-if="!claw.enabled" class="claw__flag">bị tắt</span>
        <span v-if="claw.autoStart" class="claw__flag">tự khởi động</span>
      </div>
      <div class="claw__meta">
        <span>skill: {{ claw.skill ?? '— (chỉ lịch)' }}</span>
        <span>{{ claw.status.scheduleCount }} lịch</span>
        <span v-if="nextRun">chạy kế tiếp {{ nextRun }}</span>
        <span v-if="lastRun">lần trước {{ lastRun }}</span>
        <span v-if="claw.status.restarts > 0">restart ×{{ claw.status.restarts }}</span>
      </div>
      <p v-if="claw.status.lastError" class="claw__error">{{ claw.status.lastError }}</p>
    </div>

    <div class="claw__actions">
      <button class="claw__btn" title="Sửa" :disabled="busy" @click="emit('edit')">Sửa</button>
      <button v-if="running" class="claw__btn" :disabled="busy" @click="emit('stop')">Dừng</button>
      <button v-else class="claw__btn" :disabled="busy" @click="emit('start')">Chạy</button>
      <button class="claw__btn claw__btn--danger" :disabled="busy" @click="emit('remove')">Xoá</button>
    </div>
  </div>
</template>

<style scoped>
.claw {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 10px;
  border: 1px solid #2c2c2c;
  border-radius: 6px;
  background: #1c1c1c;
}
.claw--error {
  border-color: #5a2a2a;
}
.claw__main {
  flex: 1;
  min-width: 0;
}
.claw__title {
  display: flex;
  align-items: center;
  gap: 8px;
}
.claw__state {
  padding: 1px 6px;
  border-radius: 8px;
  font-size: 11px;
  background: #262626;
  color: #9e9e9e;
}
.claw__state--running,
.claw__state--idle {
  color: #6fcf74;
}
.claw__state--error {
  color: #ff8a8a;
}
.claw__state--starting {
  color: #d3a83c;
}
.claw__flag {
  font-size: 11px;
  color: #7a7a7a;
}
.claw__meta {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  margin-top: 3px;
  font-size: 12px;
  color: #8a8a8a;
}
.claw__error {
  margin: 4px 0 0;
  font-size: 12px;
  color: #ff9b9b;
}
.claw__actions {
  display: flex;
  gap: 4px;
}
.claw__btn {
  padding: 3px 9px;
  border: 1px solid #3a3a3a;
  border-radius: 4px;
  background: #232323;
  color: inherit;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
}
.claw__btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.claw__btn--danger {
  border-color: #4a2a2a;
  color: #ff9b9b;
}
</style>
