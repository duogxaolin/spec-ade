<script setup lang="ts">
// Root component. Pha 2 scope: a sidebar (projects + lazy file tree) beside a
// main area that shows either the editor or the terminal. The real UI is a
// recursive pane/tab system with 9 tab kinds — Pha 8
// (docs/analysis/07-build-roadmap.md).

import { computed, onMounted, ref, useTemplateRef, watch } from 'vue';

import AcpPane from './components/AcpPane.vue';
import EditorPane from './components/EditorPane.vue';
import EditorTabs from './components/EditorTabs.vue';
import FileTree from './components/FileTree.vue';
import GitPanel from './components/git/GitPanel.vue';
import TerminalPane from './components/TerminalPane.vue';
import { apiFetch, resolveToken } from './api/client';
import { useAcpStore } from './stores/acp';
import { useEditorStore } from './stores/editor';
import { useProjectsStore } from './stores/projects';
import { useSettingsStore } from './stores/settings';
import { useTerminalsStore } from './stores/terminals';

const terminals = useTerminalsStore();
const projects = useProjectsStore();
const editor = useEditorStore();
const settings = useSettingsStore();
const acp = useAcpStore();

const pane = useTemplateRef<InstanceType<typeof TerminalPane>>('pane');
const editorPane = useTemplateRef<InstanceType<typeof EditorPane>>('editorPane');
const tree = useTemplateRef<InstanceType<typeof FileTree>>('tree');

const health = ref<'checking' | 'ok' | 'error'>('checking');
const serverVersion = ref('');
const notice = ref('');
/** Which surface the main area shows. Pha 8 replaces this with real panes. */
const view = ref<'editor' | 'terminal' | 'agent' | 'git'>('editor');

const activeTerminal = computed(
  () => terminals.terminals.find((t) => t.id === terminals.activeId) ?? null,
);
const projectId = computed(() => projects.activeId);

onMounted(async () => {
  // Capture `?token=` before anything else needs it.
  resolveToken();

  try {
    const body = await apiFetch<{ status: string; version: string }>('/api/health');
    health.value = body.status === 'ok' ? 'ok' : 'error';
    serverVersion.value = body.version;
  } catch (err) {
    health.value = 'error';
    notice.value = err instanceof Error ? err.message : String(err);
    return;
  }

  await Promise.all([settings.load(), projects.refresh()]);

  // Adopt shells that survived a reload before creating anything new.
  await terminals.refresh();
  if (terminals.terminals.length === 0) {
    await terminals.create(projects.active ? { cwd: projects.active.path } : {});
  }
});

// Tabs hold project-relative paths, so they are meaningless against another
// root: switching project starts from a clean slate.
watch(
  () => projects.activeId,
  (id, previous) => {
    if (previous !== undefined && id !== previous) editor.reset();
  },
);

async function openFile(path: string): Promise<void> {
  if (!projectId.value) return;
  const result = await editor.open(projectId.value, path);
  view.value = 'editor';
  // Only a fresh read carries content; an already-open tab returns null and the
  // pane still has its document.
  if (result?.kind === 'text') {
    editorPane.value?.seed(result.path, result.content);
  }
}

async function addProject(): Promise<void> {
  // No native directory picker in a browser tab: the server needs a real path,
  // and `<input type=file webkitdirectory>` never yields one. Pha 9 (Tauri) gets
  // the OS dialog; until then this is the honest input.
  const path = window.prompt('Đường dẫn thư mục project:');
  if (!path?.trim()) return;
  const project = await projects.add({ path: path.trim() });
  if (project) editor.reset();
}

async function removeProject(id: string): Promise<void> {
  const project = projects.projects.find((p) => p.id === id);
  if (!window.confirm(`Bỏ project "${project?.name ?? id}" khỏi danh sách?`)) return;
  await projects.remove(id);
}

async function newTerminal(): Promise<void> {
  // Open the new shell in the active project, falling back to the current one's
  // cwd — which is what a "new tab" does in every terminal app.
  const cwd = projects.active?.path ?? activeTerminal.value?.cwd;
  await terminals.create(cwd ? { cwd } : {});
  view.value = 'terminal';
}

function selectTerminal(id: string): void {
  terminals.select(id);
  view.value = 'terminal';
  // Wait for the pane to swap before focusing it.
  requestAnimationFrame(() => pane.value?.focus());
}

async function overwrite(): Promise<void> {
  const pending = editor.conflict;
  if (!pending || !projectId.value) return;
  await editor.overwrite(projectId.value, pending.path);
}

/** Reload the file the conflict is about, discarding the local edit. */
async function reloadConflicted(): Promise<void> {
  const pending = editor.conflict;
  if (!pending || !projectId.value) return;
  const path = pending.path;
  editor.dismissConflict();
  editor.forget(path);
  await openFile(path);
}
</script>

<template>
  <div class="app">
    <header class="app__bar">
      <strong>Spec ADE</strong>
      <span class="app__health" :class="`app__health--${health}`">
        {{ health === 'ok' ? `server ${serverVersion}` : health === 'checking' ? 'checking…' : 'backend unreachable' }}
      </span>

      <span class="app__spacer" />

      <div class="app__switch" role="tablist" aria-label="Khung làm việc">
        <button
          class="app__btn"
          :class="{ 'app__btn--on': view === 'editor' }"
          role="tab"
          :aria-selected="view === 'editor'"
          @click="view = 'editor'"
        >
          Editor
        </button>
        <button
          class="app__btn"
          :class="{ 'app__btn--on': view === 'terminal' }"
          role="tab"
          :aria-selected="view === 'terminal'"
          @click="view = 'terminal'"
        >
          Terminal
        </button>
        <button
          class="app__btn"
          :class="{ 'app__btn--on': view === 'agent' }"
          role="tab"
          :aria-selected="view === 'agent'"
          @click="view = 'agent'"
        >
          Agent
        </button>
        <button
          class="app__btn"
          :class="{ 'app__btn--on': view === 'git' }"
          role="tab"
          :aria-selected="view === 'git'"
          @click="view = 'git'"
        >
          Git
        </button>
      </div>

      <button class="app__btn" :disabled="health !== 'ok'" @click="newTerminal">
        + Terminal
      </button>
    </header>

    <div class="app__main">
      <aside class="app__side">
        <div class="app__side-head">
          <select
            class="app__select"
            aria-label="Project"
            :value="projects.activeId ?? ''"
            @change="projects.select(($event.target as HTMLSelectElement).value || null)"
          >
            <option value="" disabled>{{ projects.projects.length ? 'Chọn project' : 'Chưa có project' }}</option>
            <option v-for="p in projects.projects" :key="p.id" :value="p.id">
              {{ p.icon ? `${p.icon} ` : '' }}{{ p.name }}
            </option>
          </select>
          <button class="app__icon-btn" title="Thêm project" @click="addProject">+</button>
          <button
            v-if="projects.activeId"
            class="app__icon-btn"
            title="Bỏ project khỏi danh sách"
            @click="removeProject(projects.activeId)"
          >
            −
          </button>
        </div>

        <FileTree
          ref="tree"
          :project-id="projectId"
          :selected-path="editor.activePath"
          @open="openFile"
          @error="(message) => (notice = message)"
        />
      </aside>

      <main class="app__body">
        <template v-if="view === 'editor'">
          <p v-if="!projectId" class="app__empty">
            {{ health === 'ok' ? 'Thêm một project để bắt đầu.' : 'Đang chờ backend…' }}
          </p>
          <template v-else>
            <EditorTabs :project-id="projectId" />
            <EditorPane ref="editorPane" :project-id="projectId" />
          </template>
        </template>

        <template v-else-if="view === 'terminal'">
          <nav v-if="terminals.terminals.length" class="app__tabs">
            <button
              v-for="t in terminals.terminals"
              :key="t.id"
              class="app__tab"
              :class="{
                'app__tab--active': t.id === terminals.activeId,
                'app__tab--dead': !t.alive,
              }"
              @click="selectTerminal(t.id)"
            >
              <span>{{ t.alive ? 'sh' : `sh (exited${t.exitCode === null ? '' : ` ${t.exitCode}`})` }}</span>
              <span class="app__tab-close" role="button" @click.stop="terminals.destroy(t.id)">×</span>
            </button>
          </nav>
          <TerminalPane
            v-if="activeTerminal"
            ref="pane"
            :key="activeTerminal.id"
            :terminal-id="activeTerminal.id"
            @cwd="(path) => terminals.updateCwd(activeTerminal!.id, path)"
            @exit="(code) => terminals.markExited(activeTerminal!.id, code)"
            @error="(message) => (notice = message)"
          />
          <p v-else class="app__empty">
            {{ health === 'ok' ? 'No terminal open.' : 'Đang chờ backend…' }}
          </p>
        </template>

        <template v-else-if="view === 'git'">
          <GitPanel v-if="projectId" :project-id="projectId" />
          <p v-else class="app__empty">Thêm một project để xem Git.</p>
        </template>

        <!-- Kept mounted only while shown: its sockets hold the connection's
             watcher guard, and an unwatched connection is what the idle reaper
             is allowed to collect (SPEC-003 [INVENTED-10]). -->
        <AcpPane v-else :project-id="projectId" />
      </main>
    </div>

    <!-- A refused save is the one error the user must answer, so it gets buttons
         rather than a message that scrolls away (SPEC-002 §3.4). -->
    <footer v-if="editor.conflict" class="app__conflict">
      <span>{{ editor.conflict.path }} đã bị sửa bên ngoài.</span>
      <button class="app__btn" @click="overwrite">Ghi đè</button>
      <button class="app__btn" @click="reloadConflicted">Nạp lại từ đĩa</button>
      <button class="app__btn" @click="editor.dismissConflict()">Để sau</button>
    </footer>
    <footer
      v-else-if="notice || editor.error || projects.error || terminals.error || settings.error || acp.error"
      class="app__notice"
    >
      {{ notice || editor.error || projects.error || terminals.error || settings.error || acp.error }}
    </footer>
  </div>
</template>

<style scoped>
.app {
  display: flex;
  flex-direction: column;
  height: 100vh;
  font-family: system-ui, sans-serif;
  background: #141414;
  color: #e6e6e6;
}
.app__bar {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 12px;
  border-bottom: 1px solid #2c2c2c;
}
.app__spacer {
  flex: 1;
}
.app__switch {
  display: flex;
  gap: 4px;
}
.app__health {
  font-size: 12px;
}
.app__health--ok {
  color: #6fcf74;
}
.app__health--error {
  color: #ff8a8a;
}
.app__health--checking {
  color: #9e9e9e;
}
.app__btn {
  padding: 4px 10px;
  border: 1px solid #3a3a3a;
  border-radius: 4px;
  background: #232323;
  color: inherit;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
}
.app__btn--on {
  border-color: #4c7ecf;
  background: #26354d;
}
.app__btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}
.app__main {
  display: flex;
  flex: 1;
  min-height: 0;
}
.app__side {
  display: flex;
  flex-direction: column;
  width: 260px;
  min-width: 180px;
  border-right: 1px solid #2c2c2c;
}
.app__side-head {
  display: flex;
  gap: 4px;
  padding: 6px 8px;
  border-bottom: 1px solid #2c2c2c;
}
.app__select {
  flex: 1;
  min-width: 0;
  padding: 3px 6px;
  border: 1px solid #3a3a3a;
  border-radius: 4px;
  background: #1c1c1c;
  color: inherit;
  font: inherit;
  font-size: 12px;
}
.app__icon-btn {
  width: 24px;
  border: 1px solid #3a3a3a;
  border-radius: 4px;
  background: #232323;
  color: inherit;
  cursor: pointer;
  font: inherit;
}
.app__tabs {
  display: flex;
  gap: 2px;
  padding: 4px 8px 0;
  overflow-x: auto;
}
.app__tab {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 4px 10px;
  border: 1px solid #2c2c2c;
  border-bottom: none;
  border-radius: 4px 4px 0 0;
  background: #1c1c1c;
  color: #b8b8b8;
  cursor: pointer;
  font: inherit;
  font-size: 12px;
  white-space: nowrap;
}
.app__tab--active {
  background: #1e1e1e;
  color: #fff;
}
.app__tab--dead {
  color: #7a7a7a;
  font-style: italic;
}
.app__tab-close {
  opacity: 0.6;
}
.app__tab-close:hover {
  opacity: 1;
}
.app__body {
  display: flex;
  flex: 1;
  min-width: 0;
  min-height: 0;
  flex-direction: column;
}
.app__empty {
  padding: 2rem;
  color: #9e9e9e;
}
.app__notice {
  padding: 6px 12px;
  border-top: 1px solid #4a2020;
  background: #2a1717;
  color: #ff9b9b;
  font-size: 12px;
}
.app__conflict {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border-top: 1px solid #4a3c20;
  background: #2a2417;
  color: #ffd79b;
  font-size: 12px;
}
</style>
