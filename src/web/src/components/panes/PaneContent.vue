// kind → component map (SPEC-008 §5.8). The one place that knows which Vue
// component renders a tab. Content components keep their own lifecycles:
// Acp/Terminal mount/unmount ONLY when their tab opens/closes (never on tree
// restructuring — PaneContainer keys leaves by id); MonitorPanel keeps its
// unmount-when-hidden behaviour, INCLUDING when covered by a maximized pane
// (F31), hence the visibility wrapper.
//
// `open-location` (AcpPane) and `open` (SearchPanel) route through the shared
// openFile helper so a link click lands as "open this path (+line) in the
// focused pane" no matter which pane raised it.

<script setup lang="ts">
import { computed } from 'vue';

import AcpPane from '../AcpPane.vue';
import EditorPane from '../EditorPane.vue';
import GitPanel from '../git/GitPanel.vue';
import MonitorPanel from '../monitor/MonitorPanel.vue';
import SearchPanel from '../search/SearchPanel.vue';
import ClawsPanel from '../claws/ClawsPanel.vue';
import TerminalPane from '../TerminalPane.vue';
import { useOpenFile } from '../../panes/openFile';
import { useLayoutStore } from '../../stores/layout';
import { useTerminalsStore } from '../../stores/terminals';
import type { TabDescriptor } from '../../panes/tree';

const props = defineProps<{
  tab: TabDescriptor;
  /** Leaf that holds `tab` — the scoping key for file panes. */
  leafId: string;
  /** Project shown in this leaf's tab, or null before one is picked. */
  projectId: string | null;
  /** False while another leaf is maximized over this one (F31). */
  visible: boolean;
}>();

const openFile = useOpenFile();
const layout = useLayoutStore();
const terminals = useTerminalsStore();

/** File tabs are scoped per leaf; everything else ignores the scope. */
const editorScope = computed(() => props.leafId);

function openAt(payload: { path: string; line: number | null }): void {
  openFile(props.projectId, payload.path, payload.line);
}

/** SearchPanel emits `(path, line)` positionally, unlike AcpPane's payload. */
function openSearch(path: string, line: number): void {
  openFile(props.projectId, path, line);
}

/** Terminal id of this tab, when it is a live terminal tab. */
const terminalId = computed(() =>
  typeof props.tab.params.terminalId === 'string' ? props.tab.params.terminalId : null,
);

// A terminal's PTY outlives its pane; these keep the registry metadata current
// so the footer/status reflect cwd changes and exits without owning the socket.
function onCwd(path: string): void {
  if (terminalId.value) terminals.updateCwd(terminalId.value, path);
}
function onExit(code: number | null): void {
  if (terminalId.value) terminals.markExited(terminalId.value, code);
}
function onTerminalError(message: string): void {
  layout.setNotice(message);
}
</script>

<template>
  <div
    v-show="visible"
    class="pcontent"
  >
    <EditorPane
      v-if="tab.kind === 'file'"
      :project-id="projectId ?? ''"
      :scope="editorScope"
    />
    <TerminalPane
      v-else-if="tab.kind === 'terminal' && terminalId !== null"
      :terminal-id="terminalId"
      @cwd="onCwd"
      @exit="onExit"
      @error="onTerminalError"
    />
    <AcpPane
      v-else-if="tab.kind === 'session'"
      :project-id="projectId"
      @open-location="openAt"
    />
    <GitPanel
      v-else-if="tab.kind === 'git' && projectId !== null"
      :key="projectId"
      :project-id="projectId"
    />
    <SearchPanel
      v-else-if="tab.kind === 'search'"
      :project-id="projectId"
      @open="openSearch"
    />
    <MonitorPanel v-else-if="tab.kind === 'monitor'" />
    <ClawsPanel v-else-if="tab.kind === 'claws'" />

    <div v-else class="pcontent__missing">
      {{ tab.kind === 'terminal' ? 'Tab terminal không còn phiên nào.' : 'Không hiển thị được tab này.' }}
    </div>
  </div>
</template>

<style scoped>
.pcontent {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.pcontent > :deep(*) {
  flex: 1;
  min-height: 0;
}
.pcontent__missing {
  margin: auto;
  color: var(--text-dim, #8b8f98);
  font-size: 13px;
}
</style>
