<script setup lang="ts">
// Agent chat surface: session controls + composer (SPEC-003 §5.8, SPEC-004 §5.1).
//
// This component owns the plumbing — which agent, which session, socket state,
// sending prompts. Rendering the conversation belongs to `chat/ChatTranscript`, and
// the split matters: everything that touches agent-authored text lives under
// `components/chat/`, so the XSS surface is one directory rather than spread through
// the pane that also holds buttons.

import { computed, onBeforeUnmount, ref, watch } from 'vue';

import { useAcpStore } from '../stores/acp';
import { createSessionView } from '../stores/acpSession';
import ChatTranscript from './chat/ChatTranscript.vue';
import PermissionDialog from './chat/PermissionDialog.vue';

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

// A tool call names the files it touched, and those names should be clickable.
// Opening them in the editor needs the pane system (SPEC-008), so for now the
// click is re-emitted and the parent decides — nothing here pretends to navigate.
const emit = defineEmits<{
  (event: 'open-location', payload: { path: string; line: number | null }): void;
}>();

function onOpenLocation(payload: { path: string; line: number | null }): void {
  emit('open-location', payload);
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

      <!-- The gap notice and the plan checklist both live in ChatTranscript: they
           scroll with the conversation they describe. -->
      <ChatTranscript v-else :view="view" @open-location="onOpenLocation" />
    </div>

    <!-- A parked request blocks the agent's turn, so it gets buttons instead of
         a message that can scroll away (A9/A10). Pinned above the composer rather
         than a modal, so the diff it is asking about stays readable (04 §5.7). -->
    <PermissionDialog
      v-if="session && view.permission"
      :tool-call="view.permission.toolCall"
      :options="view.permission.options"
      @choose="(optionId: string) => session && acp.answerPermission(session.id, optionId)"
      @dismiss="session && acp.dismissPermission(session.id)"
    />

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
/* The transcript scrolls itself (it has to, to implement follow-the-bottom), so
   this is a plain flex track — an `overflow-y` here would produce two scrollbars
   and break `scrollToBottom`. */
.acp__body {
  display: flex;
  flex-direction: column;
  flex: 1;
  min-height: 0;
}
.acp__empty {
  padding: 8px 12px;
  color: #9e9e9e;
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
