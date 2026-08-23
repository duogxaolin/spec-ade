<script setup lang="ts">
// Root component (SPEC-008). The main area is a recursive pane/tab tree
// (`PaneContainer`) beside a project sidebar. App owns only the cross-cutting
// concerns the tree can't: the health check, project switching, the
// conflict/notice footer aggregated across every leaf-scoped editor store, and
// the global keyboard map (§3.5). Every surface (editor, terminal, agent, git,
// search, monitor, claws) is now a tab kind rendered by `PaneContent`, so there
// is no single `view` switch any more.

import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue';

import FileTree from './components/FileTree.vue';
import PaneContainer from './components/panes/PaneContainer.vue';
import { apiFetch, resolveToken } from './api/client';
import { useOpenFile } from './panes/openFile';
import { findLeaf, genId, leavesInOrder } from './panes/tree';
import { useAcpStore } from './stores/acp';
import { useEditorScopes, useEditorStore } from './stores/editor';
import { useLayoutStore } from './stores/layout';
import { useProjectsStore } from './stores/projects';
import { useSettingsStore } from './stores/settings';
import { useTerminalsStore } from './stores/terminals';

const terminals = useTerminalsStore();
const projects = useProjectsStore();
const settings = useSettingsStore();
const acp = useAcpStore();
const layout = useLayoutStore();
const openFile = useOpenFile();
const editorScopes = useEditorScopes();

const health = ref<'checking' | 'ok' | 'error'>('checking');
const serverVersion = ref('');
/** Gate the project watcher until the layout document has loaded (§5.2). */
const ready = ref(false);

const projectId = computed(() => projects.activeId);

/** The path of the focused leaf's active file tab, for FileTree highlighting. */
const focusedFilePath = computed<string | null>(() => {
  const t = layout.tree;
  const id = layout.activeLeafId;
  if (!t || !id) return null;
  const leaf = findLeaf(t, id);
  const tab = leaf?.tabs.find((x) => x.id === leaf.activeTabId);
  return tab && tab.kind === 'file' && typeof tab.params.path === 'string'
    ? (tab.params.path as string)
    : null;
});

// ---- conflict / error aggregation across every leaf-scoped editor store ----

/** First unresolved save conflict in any leaf, with the scope that owns it. */
const conflictInfo = computed(() => {
  for (const scope of editorScopes.value) {
    const store = useEditorStore(scope);
    if (store.conflict) return { scope, conflict: store.conflict };
  }
  return null;
});

/** First error message from any editor scope (§5.9). */
const editorError = computed<string | null>(() => {
  for (const scope of editorScopes.value) {
    const store = useEditorStore(scope);
    if (store.error) return store.error;
  }
  return null;
});

const footerMessage = computed<string | null>(
  () =>
    layout.notice ||
    editorError.value ||
    projects.error ||
    terminals.error ||
    settings.error ||
    acp.error ||
    layout.error ||
    layout.restoreNotice,
);
// ---- lifecycle -------------------------------------------------------------

onMounted(async () => {
  resolveToken();

  try {
    const body = await apiFetch<{ status: string; version: string }>('/api/health');
    health.value = body.status === 'ok' ? 'ok' : 'error';
    serverVersion.value = body.version;
  } catch (err) {
    health.value = 'error';
    layout.setNotice(err instanceof Error ? err.message : String(err));
    return;
  }

  // Layout must be loaded before any project is shown (setProject reads the
  // persisted per-project tree / lastLayout template).
  await Promise.all([settings.load(), projects.refresh(), layout.load()]);
  // Adopt shells that survived a reload; their tabs (if any) reattach on render.
  await terminals.refresh();

  ready.value = true;
  if (projects.activeId) selectProject(projects.activeId);

  window.addEventListener('keydown', onKeydown);
});

onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown));

// Project switch: show its tree; a genuinely fresh project gets one session tab.
watch(
  () => projects.activeId,
  (id) => {
    if (!ready.value) return;
    if (id) selectProject(id);
    else layout.clearCurrent();
  },
);

function selectProject(id: string): void {
  layout.setProject(id);
  const t = layout.tree;
  // Fresh project (empty leaves, no persisted tabs) → seed the agent tab (§5.9).
  if (t && leavesInOrder(t).every((l) => l.tabs.length === 0)) {
    layout.openSingleton('session');
  }
}
// ---- sidebar actions -------------------------------------------------------

async function addProject(): Promise<void> {
  // No native directory picker in a browser tab: the server needs a real path.
  const path = window.prompt('Đường dẫn thư mục project:');
  if (!path?.trim()) return;
  await projects.add({ path: path.trim() });
}

async function removeProject(id: string): Promise<void> {
  const project = projects.projects.find((p) => p.id === id);
  if (!window.confirm(`Bỏ project "${project?.name ?? id}" khỏi danh sách?`)) return;
  await projects.remove(id);
  // Drop its tree from the mirror so the next PUT doesn't resurrect it (F24).
  layout.dropProject(id);
}

async function newTerminal(): Promise<void> {
  // Open the new shell in the active project's cwd, then give it a tab in the
  // focused leaf — "new terminal tab", the same as every terminal app.
  const cwd = projects.active?.path;
  const info = await terminals.create(cwd ? { cwd } : {});
  if (info) {
    layout.openTab({
      id: genId('tab'),
      kind: 'terminal',
      title: 'sh',
      params: { terminalId: info.id },
    });
  }
}
// ---- conflict / notice footer ----------------------------------------------

async function overwrite(): Promise<void> {
  const info = conflictInfo.value;
  if (!info || !projectId.value) return;
  await useEditorStore(info.scope).overwrite(projectId.value, info.conflict.path);
}

/** Discard the local edit and re-read from disk, in the leaf that owns it. */
function reloadConflicted(): void {
  const info = conflictInfo.value;
  if (!info) return;
  const store = useEditorStore(info.scope);
  store.dismissConflict();
  store.requestReload(info.conflict.path);
}

function dismissConflict(): void {
  const info = conflictInfo.value;
  if (info) useEditorStore(info.scope).dismissConflict();
}

function dismissNotice(): void {
  layout.clearNotice();
  layout.clearMissingFiles();
}

// ---- global keyboard map (§3.5) --------------------------------------------

function onKeydown(ev: KeyboardEvent): void {
  if (!(ev.metaKey || ev.ctrlKey)) return;
  switch (ev.code) {
    case 'Backslash': // ⌘\ split right, ⌘⇧\ split down
      ev.preventDefault();
      layout.splitActive(ev.shiftKey ? 'vertical' : 'horizontal');
      break;
    case 'BracketRight': // ⌘] focus next leaf
      ev.preventDefault();
      layout.cycleFocus(1);
      break;
    case 'BracketLeft': // ⌘[ focus previous leaf
      ev.preventDefault();
      layout.cycleFocus(-1);
      break;
    case 'Enter': // ⌘⇧↵ toggle maximize
      if (ev.shiftKey) {
        ev.preventDefault();
        layout.toggleMaximize();
      }
      break;
    case 'KeyW': // ⌘W close focused pane's active tab
      ev.preventDefault();
      void closeFocused();
      break;
    default:
  }
}
/** Close the focused leaf's active tab, saving a dirty file first (§5.6). */
async function closeFocused(): Promise<void> {
  const t = layout.tree;
  const id = layout.activeLeafId;
  if (!t || !id) return;
  const leaf = findLeaf(t, id);
  const tabId = leaf?.activeTabId;
  if (!leaf || !tabId) return;
  const tab = leaf.tabs.find((x) => x.id === tabId);
  if (tab?.kind === 'file' && projectId.value && typeof tab.params.path === 'string') {
    await useEditorStore(id).saveIfDirty(projectId.value, tab.params.path);
  }
  layout.closeTab(id, tabId);
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

      <div class="app__switch" role="toolbar" aria-label="Mở panel">
        <button class="app__btn" :disabled="!projectId" @click="layout.openSingleton('session')">Agent</button>
        <button class="app__btn" :disabled="!projectId" @click="layout.openSingleton('git')">Git</button>
        <button class="app__btn" :disabled="!projectId" @click="layout.openSingleton('search')">Tìm</button>
        <button class="app__btn" :disabled="!projectId" @click="layout.openSingleton('monitor')">Máy</button>
        <button class="app__btn" :disabled="!projectId" @click="layout.openSingleton('claws')">Claws</button>
      </div>

      <button class="app__btn" :disabled="health !== 'ok' || !projectId" @click="newTerminal">
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
          :project-id="projectId"
          :selected-path="focusedFilePath"
          @open="(path) => openFile(projectId, path)"
          @error="(message) => layout.setNotice(message)"
        />
      </aside>

      <main class="app__body">
        <PaneContainer v-if="layout.tree" :node="layout.tree" />
        <p v-else class="app__empty">
          {{ health === 'ok' ? 'Thêm một project để bắt đầu.' : 'Đang chờ backend…' }}
        </p>
      </main>
    </div>
    <!-- A refused save is the one error the user must answer, so it gets buttons
         rather than a message that scrolls away (SPEC-002 §3.4). The conflict may
         belong to any leaf — `conflictInfo` carries the scope that owns it. -->
    <footer v-if="conflictInfo" class="app__conflict">
      <span>{{ conflictInfo.conflict.path }} đã bị sửa bên ngoài.</span>
      <button class="app__btn" @click="overwrite">Ghi đè</button>
      <button class="app__btn" @click="reloadConflicted">Nạp lại từ đĩa</button>
      <button class="app__btn" @click="dismissConflict">Để sau</button>
    </footer>
    <footer v-else-if="footerMessage" class="app__notice">
      <span>{{ footerMessage }}</span>
      <button class="app__notice-x" title="Bỏ qua" @click="dismissNotice">×</button>
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
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 12px;
  border-top: 1px solid #4a2020;
  background: #2a1717;
  color: #ff9b9b;
  font-size: 12px;
}
.app__notice span {
  flex: 1;
}
.app__notice-x {
  border: 0;
  background: transparent;
  color: inherit;
  cursor: pointer;
  font: inherit;
  font-size: 14px;
  line-height: 1;
  padding: 0 4px;
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
