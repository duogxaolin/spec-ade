<script setup lang="ts">
// The claws panel — 7th tab (SPEC-007 §5.8): list + form + start/stop.
//
// Owns every store call; `ClawForm` and `ClawRow` are props-in/events-out. The
// panel refreshes on mount and after each mutation adopts the server's row, so
// the state badge is always what the *runtime* reports, never a local guess.
//
// Error detail comes from `ApiError.body.detail` via the store; cron errors add
// `schedule: <index>` (§3.2), surfaced here next to the message so the user can
// find the offending row in the form.

import { computed, onMounted, ref } from 'vue';

import type { ClawInput, ClawRow as ClawRowData } from '../../api/claws';
import { useAcpStore } from '../../stores/acp';
import { useClawsStore } from '../../stores/claws';
import { useProjectsStore } from '../../stores/projects';
import ClawForm from './ClawForm.vue';
import ClawRow from './ClawRow.vue';

const claws = useClawsStore();
const projects = useProjectsStore();
const acp = useAcpStore();

/** Which claw's form is open (`null` = closed, `'new'` = creating). */
const editing = ref<string | null>(null);
const busyId = ref<string | null>(null);

const projectOptions = computed(() =>
  projects.projects.map((p) => ({ id: p.id, name: p.icon ? `${p.icon} ${p.name}` : p.name })),
);

const agentOptions = computed(() => acp.agents.map((a) => ({ id: a.id, name: a.name })));

const visibleClaws = computed(() => {
  const pid = projects.activeId;
  return pid ? claws.forProject(pid) : claws.claws;
});

function openCreate(): void {
  if (projectOptions.value.length === 0 || agentOptions.value.length === 0) return;
  editing.value = 'new';
  // `:key` remounts the form per target; the fill/reset happens after mount.
  requestAnimationFrame(() => formRef.value?.openCreate());
}

const formRef = ref<InstanceType<typeof ClawForm> | null>(null);

function openEdit(row: ClawRowData): void {
  editing.value = row.id;
  requestAnimationFrame(() => formRef.value?.openEdit(row));
}

async function submit(value: ClawInput): Promise<void> {
  if (editing.value === 'new') {
    const row = await claws.add(value);
    if (row) closeForm();
  } else if (editing.value !== null) {
    const id = editing.value;
    const row = await claws.save(id, value);
    // A running claw restarts onto the new config server-side; the saved row
    // already carries the post-restart status.
    if (row) closeForm();
  }
}

function closeForm(): void {
  editing.value = null;
}

async function onStart(row: ClawRowData): Promise<void> {
  busyId.value = row.id;
  try {
    await claws.start(row.id);
  } finally {
    busyId.value = null;
  }
}

async function onStop(row: ClawRowData): Promise<void> {
  busyId.value = row.id;
  try {
    await claws.stop(row.id);
  } finally {
    busyId.value = null;
  }
}

async function onRemove(row: ClawRowData): Promise<void> {
  if (!window.confirm(`Xoá Claw "${row.name}"? Lịch sẽ ngưng chạy vĩnh viễn.`)) return;
  busyId.value = row.id;
  try {
    await claws.remove(row.id);
    if (editing.value === row.id) closeForm();
  } finally {
    busyId.value = null;
  }
}

onMounted(async () => {
  await Promise.all([claws.refresh(), acp.refresh(projects.activeId ?? undefined)]);
});
</script>

<template>
  <div class="claws">
    <header class="claws__head">
      <strong>Claws — agent tự động theo lịch</strong>
      <span class="claws__spacer" />
      <button
        class="claws__btn"
        :disabled="projectOptions.length === 0 || agentOptions.length === 0"
        :title="
          projectOptions.length === 0
            ? 'Cần một project'
            : agentOptions.length === 0
              ? 'Chưa có agent nào trong catalogue'
              : ''
        "
        @click="openCreate"
      >
        + Claw
      </button>
    </header>

    <p v-if="claws.error" class="claws__error">{{ claws.error }}</p>
    <p v-else-if="!claws.hasAny && !claws.loading" class="claws__empty">
      Chưa có Claw nào. Một Claw chạy một skill theo lịch cron bằng agent của bạn.
    </p>

    <ClawRow
      v-for="row in visibleClaws"
      :key="row.id"
      :claw="row"
      :busy="busyId === row.id || claws.busyId === row.id"
      @start="onStart(row)"
      @stop="onStop(row)"
      @remove="onRemove(row)"
      @edit="openEdit(row)"
    />

    <!-- One form instance: `openEdit` fills it from the row being edited,
         `openCreate` resets it. Keeping a single mounted form avoids the
         two-branches-two-refs problem and keeps draft state in one place. -->
    <ClawForm
      v-if="editing !== null"
      ref="formRef"
      :key="editing"
      :agents="agentOptions"
      :projects="projectOptions"
      @submit="submit"
      @cancel="closeForm"
    />
  </div>
</template>

<style scoped>
.claws {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px;
  overflow-y: auto;
}
.claws__head {
  display: flex;
  align-items: center;
  gap: 8px;
}
.claws__spacer {
  flex: 1;
}
.claws__btn {
  padding: 4px 10px;
  border: 1px solid #3a3a3a;
  border-radius: 4px;
  background: #232323;
  color: inherit;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
}
.claws__btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.claws__error {
  margin: 0;
  padding: 6px 8px;
  border: 1px solid #4a2020;
  border-radius: 4px;
  background: #2a1717;
  color: #ff9b9b;
  font-size: 12px;
}
.claws__empty {
  margin: 0;
  color: #9e9e9e;
}
</style>
