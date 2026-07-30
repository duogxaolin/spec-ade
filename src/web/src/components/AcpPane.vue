<script setup lang="ts">
// Raw agent chat surface (SPEC-003 §5.8).
//
// Deliberately unstyled-by-Quasar and markdown-free: this phase proves the ACP
// plumbing end to end (prompt → chunks → tool calls → permission → turn end).
// The real chat UI — markdown, code blocks, diff views, streaming cursor — is
// SPEC-004, and building it here would mean rewriting it there.

import { computed, onBeforeUnmount, ref, watch } from 'vue';

import { useAcpStore } from '../stores/acp';
import { createSessionView } from '../stores/acpSession';

const props = defineProps<{
  /** Which project this pane's agent runs in. Null until one is selected. */
  projectId: string | null;
}>();

const acp = useAcpStore();
const draft = ref('');
/** Which agent to spawn; the first configured one, once the catalogue loads. */
const agentId = ref('');

const session = computed(() =>
  acp.sessions.find((s) => s.id === acp.activeSessionId) ?? null,
);
// A missing view would mean every `v-if` in the template needs a guard, so an
// empty one stands in until a session is attached.
const view = computed(() => acp.activeView ?? createSessionView());
const socketState = computed(() =>
  acp.activeSessionId ? (acp.socketStates[acp.activeSessionId] ?? 'closed') : 'closed',
);
const canSend = computed(
  () => Boolean(session.value) && !view.value.turnActive && view.value.state !== 'closed',
);

watch(
  () => props.projectId,
  async (id) => {
    // Sessions are per project and a transcript from another root is
    // meaningless, so switching drops every socket before reloading.
    acp.disposeAll();
    if (!id) return;
    await acp.refresh(id);
    agentId.value = acp.agents[0]?.id ?? '';
    // Re-adopt what the server still has: connections and sessions outlive the
    // page, so a reload must reattach instead of starting a second agent.
    for (const existing of acp.sessions) acp.attach(existing);
    if (!acp.activeSessionId && acp.sessions.length) acp.select(acp.sessions[0]!.id);
  },
  { immediate: true },
);

// Sockets are not owned by the component tree, but this pane is the only thing
// reading them in Pha 3 — leaving them open after unmount would keep the
// connection looking watched and block the idle reaper ([INVENTED-10]).
onBeforeUnmount(() => acp.disposeAll());

async function startSession(): Promise<void> {
  if (!props.projectId || !agentId.value) return;
  const connectionId = await acp.ensureConnection(agentId.value, props.projectId);
  if (!connectionId) return;
  await acp.openSession(props.projectId, connectionId);
}

function send(): void {
  if (!session.value || !canSend.value) return;
  acp.prompt(session.value.id, draft.value);
  draft.value = '';
}

/** Enter sends; Shift+Enter is a newline, as in every chat client. */
function onKeydown(event: KeyboardEvent): void {
  if (event.key === 'Enter' && !event.shiftKey) {
    event.preventDefault();
    send();
  }
}

function toolLabel(toolCallId: string): string {
  const call = view.value.toolCalls[toolCallId];
  if (!call) return toolCallId;
  const status = call.status ? ` · ${call.status}` : '';
  return `${call.title ?? call.kind ?? toolCallId}${status}`;
}
</script>

<template>
  <section class="acp">
    <header class="acp__bar">
      <select
        v-model="agentId"
        class="acp__select"
        aria-label="Agent"
        :disabled="!acp.agents.length"
      >
        <option v-if="!acp.agents.length" value="">Chưa cấu hình agent</option>
        <option v-for="a in acp.agents" :key="a.id" :value="a.id">{{ a.name }}</option>
      </select>
      <button
        class="acp__btn"
        :disabled="!projectId || !agentId"
        @click="startSession"
      >
        + Session
      </button>

      <select
        v-if="acp.sessions.length"
        class="acp__select"
        aria-label="Session"
        :value="acp.activeSessionId ?? ''"
        @change="acp.select(($event.target as HTMLSelectElement).value)"
      >
        <option v-for="s in acp.sessions" :key="s.id" :value="s.id">
          {{ s.id.slice(0, 8) }}
        </option>
      </select>

      <span class="acp__spacer" />

      <span class="acp__state" :class="`acp__state--${socketState}`">{{ socketState }}</span>
      <button
        v-if="session"
        class="acp__btn"
        title="Đóng session (agent vẫn chạy)"
        @click="acp.closeSession(session.id)"
      >
        ×
      </button>
    </header>

    <div class="acp__body">
      <p v-if="!session" class="acp__empty">
        {{ projectId ? 'Chưa có session. Bấm + Session để bắt đầu.' : 'Chọn một project trước.' }}
      </p>

      <template v-else>
        <!-- A pruned log means the transcript is not the whole conversation.
             Saying so beats letting the user read a gap as a complete exchange. -->
        <p v-if="view.hasGap" class="acp__gap-note">
          Một phần lịch sử đã bị xoá bởi server.
        </p>

        <ol v-if="view.plan?.entries.length" class="acp__plan">
          <li v-for="(step, i) in view.plan.entries" :key="i" :data-status="step.status">
            {{ step.content }}
          </li>
        </ol>

        <div class="acp__stream">
          <template v-for="entry in view.entries" :key="`${entry.kind}-${entry.seq}`">
            <p v-if="entry.kind === 'message'" class="acp__msg">{{ entry.text }}</p>
            <p v-else-if="entry.kind === 'thought'" class="acp__thought">{{ entry.text }}</p>
            <p v-else-if="entry.kind === 'tool'" class="acp__tool">
              {{ toolLabel(entry.toolCallId) }}
            </p>
            <p v-else-if="entry.kind === 'turn_end'" class="acp__end">
              {{ entry.label || '— hết lượt —' }}
            </p>
            <p v-else-if="entry.kind === 'gap'" class="acp__end">
              — thiếu lịch sử trước seq {{ entry.fromSeq }} —
            </p>
            <p v-else class="acp__end">{{ entry.text }}</p>
          </template>
        </div>
      </template>
    </div>

    <!-- A parked request blocks the agent's turn, so it gets buttons instead of
         a message that can scroll away (A9/A10). -->
    <div v-if="view.permission" class="acp__permission">
      <span>{{ view.permission.toolCall.title ?? 'Agent xin quyền' }}</span>
      <button
        v-for="opt in view.permission.options"
        :key="opt.optionId"
        class="acp__btn"
        @click="session && acp.answerPermission(session.id, opt.optionId)"
      >
        {{ opt.name }}
      </button>
      <button
        class="acp__btn"
        @click="session && acp.dismissPermission(session.id)"
      >
        Bỏ qua
      </button>
    </div>

    <footer class="acp__input">
      <textarea
        v-model="draft"
        class="acp__textarea"
        rows="2"
        :disabled="!canSend"
        :placeholder="view.turnActive ? 'Agent đang trả lời…' : 'Nhập prompt (Enter để gửi)'"
        aria-label="Prompt"
        @keydown="onKeydown"
      />
      <button v-if="view.turnActive" class="acp__btn" @click="session && acp.cancel(session.id)">
        Dừng
      </button>
      <button v-else class="acp__btn" :disabled="!canSend || !draft.trim()" @click="send">
        Gửi
      </button>
    </footer>
  </section>
</template>

<style scoped>
.acp {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
}
.acp__bar {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 6px 8px;
  border-bottom: 1px solid #2c2c2c;
}
.acp__spacer {
  flex: 1;
}
.acp__select {
  padding: 3px 6px;
  border: 1px solid #3a3a3a;
  border-radius: 4px;
  background: #1c1c1c;
  color: inherit;
  font: inherit;
  font-size: 12px;
}
.acp__btn {
  padding: 4px 10px;
  border: 1px solid #3a3a3a;
  border-radius: 4px;
  background: #232323;
  color: inherit;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
}
.acp__btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.acp__state {
  font-size: 11px;
  color: #9e9e9e;
}
.acp__state--open {
  color: #6fcf74;
}
.acp__state--reconnecting {
  color: #ffd24a;
}
.acp__body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 8px 12px;
}
.acp__empty {
  color: #9e9e9e;
}
.acp__stream {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
/* Agent replies arrive with meaningful newlines and indentation; collapsing
   whitespace would mangle every code block it emits. */
.acp__msg,
.acp__thought {
  margin: 0;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}
.acp__thought {
  color: #9e9e9e;
  font-style: italic;
}
.acp__tool {
  margin: 0;
  padding: 2px 6px;
  border-left: 2px solid #4c7ecf;
  color: #b8cdf0;
  font-family: ui-monospace, Menlo, Consolas, monospace;
  font-size: 12px;
}
.acp__end {
  margin: 0;
  color: #7a7a7a;
  font-size: 11px;
}
.acp__gap-note {
  margin: 0 0 8px;
  color: #ffd79b;
  font-size: 12px;
}
.acp__plan {
  margin: 0 0 8px;
  padding-left: 20px;
  color: #c9c9c9;
  font-size: 12px;
}
.acp__plan li[data-status='completed'] {
  color: #6fcf74;
  text-decoration: line-through;
}
.acp__permission {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border-top: 1px solid #4a3c20;
  background: #2a2417;
  color: #ffd79b;
  font-size: 12px;
}
.acp__input {
  display: flex;
  align-items: flex-end;
  gap: 8px;
  padding: 8px;
  border-top: 1px solid #2c2c2c;
}
.acp__textarea {
  flex: 1;
  min-width: 0;
  padding: 6px 8px;
  border: 1px solid #3a3a3a;
  border-radius: 4px;
  background: #1c1c1c;
  color: inherit;
  font: inherit;
  font-size: 13px;
  resize: vertical;
}
</style>
