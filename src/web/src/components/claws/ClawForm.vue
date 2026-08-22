<script setup lang="ts">
// The create/edit claw form (SPEC-007 §3.1, §9.1).
//
// One deliberate warning lives here and nowhere else: choosing `auto_approve`
// tells the agent "yes" to every permission request without asking a human —
// that is execution authority, not a UI preference (§9.1). The warning shows
// whenever the mode is selected, not only on save, because the dangerous state
// is *having* the mode armed, not clicking submit.
//
// Schedules are edited as raw cron strings; the server validates on save
// (`claws.mdx:70`) and answers 400 with `schedule: <index>` (§3.2), which the
// panel surfaces next to the offending row.

import { computed, reactive, ref } from 'vue';

import type { ClawInput, ClawRow, PermissionMode } from '../../api/claws';

const props = defineProps<{
  agents: { id: string; name: string }[];
  projects: { id: string; name: string }[];
}>();

/** The form's editing shape — prompts as one textarea string, not an array. */
export interface FormValue {
  name: string;
  agentId: string;
  projectId: string;
  skill: string | null;
  enabled: boolean;
  autoStart: boolean;
  keepAlive: boolean;
  restartOnTrigger: boolean;
  permissionMode: PermissionMode;
  skipIfRunning: boolean;
  schedules: {
    label: string;
    cron: string;
    promptsText: string;
    enabled: boolean;
  }[];
}

const emit = defineEmits<{
  submit: [value: ClawInput];
  cancel: [];
}>();

const MODES: { value: PermissionMode; label: string }[] = [
  { value: 'auto_approve', label: 'auto_approve — tự đồng ý mọi yêu cầu' },
  { value: 'deny_all', label: 'deny_all — từ chối tất cả' },
  { value: 'ask_via_ui', label: 'ask_via_ui — hỏi qua UI' },
];

/** Blank form for create; `edit` fills it from an existing row. */
function blank(): FormValue {
  return {
    name: '',
    agentId: props.agents[0]?.id ?? '',
    projectId: props.projects[0]?.id ?? '',
    skill: null,
    enabled: true,
    autoStart: false,
    keepAlive: true,
    restartOnTrigger: false,
    permissionMode: 'auto_approve',
    skipIfRunning: true,
    schedules: [],
  };
}

const form = reactive<FormValue>(blank());
/** Which existing claw is being edited, if any — drives the title. */
const editingId = ref<string | null>(null);

function openEdit(row: ClawRow): void {
  editingId.value = row.id;
  Object.assign(form, {
    name: row.name,
    agentId: row.agentId,
    projectId: row.projectId,
    skill: row.skill,
    enabled: row.enabled,
    autoStart: row.autoStart,
    keepAlive: row.keepAlive,
    restartOnTrigger: row.restartOnTrigger,
    permissionMode: row.permissionMode,
    skipIfRunning: row.skipIfRunning,
    schedules: row.schedules.map((s) => ({
      label: s.label ?? '',
      cron: s.cron,
      promptsText: s.prompts.join('\n'),
      enabled: s.enabled,
    })),
  });
}

function openCreate(): void {
  editingId.value = null;
  Object.assign(form, blank());
}

defineExpose({ openCreate, openEdit });

/** §9.1: the auto-approve warning is unconditional while the mode is selected. */
const warnAutoApprove = computed(() => form.permissionMode === 'auto_approve');

const canSubmit = computed(
  () =>
    form.name.trim() !== '' &&
    form.agentId !== '' &&
    props.projects.length > 0 &&
    props.agents.length > 0,
);

function addSchedule(): void {
  form.schedules.push({ label: '', cron: '', promptsText: '', enabled: true });
}

function removeSchedule(index: number): void {
  form.schedules.splice(index, 1);
}

/** Prompts arrive one per line; blanks are dropped exactly like the server does. */
function buildPayload(): ClawInput {
  return {
    name: form.name.trim(),
    agentId: form.agentId,
    projectId: form.projectId,
    skill: form.skill?.trim() ? form.skill.trim() : null,
    enabled: form.enabled,
    autoStart: form.autoStart,
    keepAlive: form.keepAlive,
    restartOnTrigger: form.restartOnTrigger,
    permissionMode: form.permissionMode,
    skipIfRunning: form.skipIfRunning,
    schedules: form.schedules.map((s) => ({
      label: s.label.trim() || null,
      cron: s.cron.trim(),
      prompts: s.promptsText
        .split('\n')
        .map((p) => p.trim())
        .filter((p) => p !== ''),
      enabled: s.enabled,
    })),
  };
}

function submit(): void {
  if (!canSubmit.value) return;
  emit('submit', buildPayload());
}
</script>

<template>
  <form class="form" @submit.prevent="submit">
    <h3 class="form__title">{{ editingId ? 'Sửa Claw' : 'Claw mới' }}</h3>

    <div class="form__grid">
      <label class="form__field">
        <span>Tên</span>
        <input v-model="form.name" class="form__input" placeholder="review-bot" />
      </label>

      <label class="form__field">
        <span>Agent</span>
        <select v-model="form.agentId" class="form__input">
          <option v-for="a in agents" :key="a.id" :value="a.id">{{ a.name }}</option>
        </select>
      </label>

      <label class="form__field">
        <span>Project</span>
        <select v-model="form.projectId" class="form__input">
          <option v-for="p in projects" :key="p.id" :value="p.id">{{ p.name }}</option>
        </select>
      </label>

      <label class="form__field">
        <span>Skill (tuỳ chọn)</span>
        <input v-model="form.skill" class="form__input" placeholder="để trống nếu chỉ chạy lịch" />
      </label>
    </div>

    <div class="form__checks">
      <label><input v-model="form.enabled" type="checkbox" /> bật</label>
      <label><input v-model="form.autoStart" type="checkbox" /> tự khởi động khi mở server</label>
      <label><input v-model="form.keepAlive" type="checkbox" /> giữ sống (tối đa 3 lần restart)</label>
      <label><input v-model="form.restartOnTrigger" type="checkbox" /> kết nối mới mỗi lần lịch bắn</label>
      <label><input v-model="form.skipIfRunning" type="checkbox" /> bỏ nhịp khi đang chạy</label>
    </div>

    <label class="form__field">
      <span>Chế độ quyền</span>
      <select v-model="form.permissionMode" class="form__input">
        <option v-for="m in MODES" :key="m.value" :value="m.value">{{ m.label }}</option>
      </select>
      <!-- §9.1: this must be visible at selection time — the mode grants the
           agent authority to execute tools with no human in the loop. -->
      <p v-if="warnAutoApprove" class="form__warning" role="alert">
        Cảnh báo: auto_approve cấp quyền thực thi cho agent mà không hỏi lại —
        mọi yêu cầu quyền sẽ được tự động chấp nhận.
      </p>
    </label>

    <div class="form__schedules">
      <div class="form__schedhead">
        <strong>Lịch</strong>
        <button type="button" class="form__btn" @click="addSchedule">+ lịch</button>
      </div>
      <p v-if="form.schedules.length === 0" class="form__hint">
        Không có lịch nào — Claw chỉ khởi động thủ công.
      </p>
      <div v-for="(s, i) in form.schedules" :key="i" class="form__sched">
        <input v-model="s.label" class="form__input form__input--slim" placeholder="nhãn (tuỳ chọn)" />
        <input v-model="s.cron" class="form__input form__input--cron" placeholder="cron, ví dụ 0 9 * * *" />
        <textarea
          v-model="s.promptsText"
          class="form__input form__input--prompts"
          rows="2"
          placeholder="một prompt mỗi dòng"
        />
        <label class="form__check"><input v-model="s.enabled" type="checkbox" /> bật</label>
        <button type="button" class="form__btn form__btn--danger" @click="removeSchedule(i)">xoá</button>
      </div>
    </div>

    <div class="form__actions">
      <button type="submit" class="form__btn form__btn--primary" :disabled="!canSubmit">
        {{ editingId ? 'Lưu' : 'Tạo' }}
      </button>
      <button type="button" class="form__btn" @click="emit('cancel')">Huỷ</button>
    </div>
  </form>
</template>

<style scoped>
.form {
  display: flex;
  flex-direction: column;
  gap: 10px;
  padding: 12px;
  border: 1px solid #2c2c2c;
  border-radius: 6px;
  background: #1c1c1c;
}
.form__title {
  margin: 0;
  font-size: 14px;
}
.form__grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 8px;
}
.form__field {
  display: flex;
  flex-direction: column;
  gap: 4px;
  font-size: 12px;
  color: #9e9e9e;
}
.form__input {
  padding: 4px 8px;
  border: 1px solid #3a3a3a;
  border-radius: 4px;
  background: #141414;
  color: inherit;
  font: inherit;
  font-size: 12px;
}
.form__input--slim {
  width: 140px;
}
.form__input--cron {
  width: 180px;
  font-family: ui-monospace, monospace;
}
.form__input--prompts {
  flex: 1;
  min-width: 200px;
  resize: vertical;
}
.form__checks {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  font-size: 12px;
  color: #9e9e9e;
}
.form__warning {
  margin: 4px 0 0;
  padding: 6px 8px;
  border: 1px solid #5a4a1e;
  border-radius: 4px;
  background: #2a2417;
  color: #ffd79b;
  font-size: 12px;
}
.form__schedules {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.form__schedhead {
  display: flex;
  align-items: center;
  justify-content: space-between;
  font-size: 13px;
}
.form__hint {
  margin: 0;
  color: #7a7a7a;
  font-size: 12px;
}
.form__sched {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  padding: 6px;
  border: 1px solid #262626;
  border-radius: 4px;
}
.form__check {
  font-size: 12px;
  color: #9e9e9e;
}
.form__actions {
  display: flex;
  gap: 8px;
}
.form__btn {
  padding: 4px 12px;
  border: 1px solid #3a3a3a;
  border-radius: 4px;
  background: #232323;
  color: inherit;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
}
.form__btn--primary {
  border-color: #4c7ecf;
  background: #26354d;
}
.form__btn--danger {
  border-color: #4a2a2a;
  color: #ff9b9b;
}
.form__btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
</style>
