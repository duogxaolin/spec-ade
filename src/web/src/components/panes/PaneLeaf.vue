// One pane leaf (SPEC-008 §5.1/§5.5/§5.6): tab strip on top, the active
// tab's content below, focus chrome, and the drop-preview overlay.
//
// Focus (§5.5) has TWO setters: `pointerdown` capture (clicks land even when
// an inner target stops propagation, e.g. inside CodeMirror) and `focusin`
// (keyboard navigation into an input focuses the pane without any mouse
// event). The focused leaf keeps its accent underline; unfocused leaves dim
// their ACTION CHROME, never their content.
//
// Maximize (F31): a covered leaf stays MOUNTED but `display:none` — socket
// components (Acp/Terminal) must survive covering; the one exception is
// MonitorPanel, which PaneContent unmounts via the `visible` prop.
//
// Closing a file tab saves first via the leaf's scoped editor store; every
// other kind closes straight through the layout store.

<script setup lang="ts">
import { computed } from 'vue';

import type { PaneLeaf as LeafNode, PanePath } from '../../panes/tree';
import { useEditorStore } from '../../stores/editor';
import { useLayoutStore } from '../../stores/layout';
import PaneContentVue from './PaneContent.vue';
import PaneTabStripVue from './PaneTabStrip.vue';

const props = defineProps<{
  leaf: LeafNode;
  path: PanePath;
}>();

const layout = useLayoutStore();

const activeTab = computed(
  () => props.leaf.tabs.find((t) => t.id === props.leaf.activeTabId) ?? null,
);

const focused = computed(() => layout.activeLeafId === props.leaf.id);

/** Another leaf is maximized over this one → hidden, still mounted (F31). */
const coveredByMaximize = computed(() => {
  const max = layout.maximizedLeafId;
  return max !== null && max !== props.leaf.id;
});

function pathOf(tab: { params: Record<string, unknown> }): string {
  return typeof tab.params.path === 'string' ? tab.params.path : '';
}

/**
 * Close with dirty-file handshake: file tabs save first via the leaf's
 * scoped editor store (same rule as switching away); other kinds close
 * straight through the tree op, which also auto-unplits an emptied leaf.
 */
async function onClose(tabId: string): Promise<void> {
  const tab = props.leaf.tabs.find((t) => t.id === tabId);
  if (!tab) return;
  const pid = layout.currentProjectId;
  if (tab.kind === 'file' && pid) {
    await useEditorStore(props.leaf.id).saveIfDirty(pid, pathOf(tab));
  }
  layout.closeTab(props.leaf.id, tabId);
}

/** Drop outcome from the strip: reorder/move, or split toward the zone. */
function onDrop(
  payload:
    | { kind: 'move'; tabId: string; toLeafId: string; toIndex: number }
    | { kind: 'split'; tabId: string; zone: 'left' | 'right' | 'up' | 'down' },
): void {
  if (payload.kind === 'move') {
    layout.moveTab(payload.tabId, payload.tabId, payload.toLeafId, Math.max(0, payload.toIndex));
    return;
  }
  // Split toward the zone relative to THIS leaf, then move the dragged tab
  // into the freshly created half (which takes focus per splitLeafAt).
  const direction = payload.zone === 'left' || payload.zone === 'right' ? 'horizontal' : 'vertical';
  const side = payload.zone === 'left' || payload.zone === 'up' ? 'first' : 'second';
  const newLeafId = layout.splitLeafAt(props.leaf.id, direction, side);
  if (!newLeafId) return;
  layout.moveTab(payload.tabId, payload.tabId, newLeafId, 0);
}

function toggleMaximize(): void {
  // Maximize acts on the FOCUSED leaf — clicking the button first focuses
  // this one so the toggle is never applied to the wrong pane.
  if (!focused.value) layout.focusLeaf(props.leaf.id);
  layout.toggleMaximize();
}
</script>

<template>
  <section
    class="pleaf"
    :class="{ 'pleaf--focused': focused, 'pleaf--covered': coveredByMaximize }"
    :data-pane-leaf="leaf.id"
    @pointerdown.capture="layout.focusLeaf(leaf.id)"
    @focusin="layout.focusLeaf(leaf.id)"
  >
    <header class="pleaf__strip">
      <PaneTabStripVue
        :leaf-id="leaf.id"
        :tabs="leaf.tabs"
        :active-tab-id="leaf.activeTabId"
        @activate="(id) => layout.activateTab(leaf.id, id)"
        @close="onClose"
        @drop="onDrop"
      />
      <div class="pleaf__actions">
        <button class="pleaf__action" title="Chia đôi sang phải" @click="layout.splitActive('horizontal')">│</button>
        <button class="pleaf__action" title="Chia đôi xuống dưới" @click="layout.splitActive('vertical')">━</button>
        <button class="pleaf__action" title="Phóng to / thu nhỏ pane" @click="toggleMaximize">⤢</button>
      </div>
    </header>

    <div class="pleaf__body">
      <PaneContentVue
        v-if="activeTab"
        :tab="activeTab"
        :leaf-id="leaf.id"
        :project-id="layout.currentProjectId"
        :visible="!coveredByMaximize"
      />
      <div v-else class="pleaf__empty">Trống — mở một tab từ thanh công cụ hoặc kéo tab vào đây.</div>
    </div>
  </section>
</template>

<style scoped>
.pleaf {
  position: relative;
  display: flex;
  flex-direction: column;
  min-width: 0;
  min-height: 0;
  height: 100%;
  background: var(--bg, #16171b);
}
/* Covered by a maximized sibling: out of sight, still mounted (F31). */
.pleaf--covered {
  display: none;
}
.pleaf__strip {
  display: flex;
  align-items: center;
  flex: none;
}
.pleaf--focused .pleaf__strip {
  box-shadow: inset 0 -2px 0 var(--accent, #4f8cff);
}
.pleaf__actions {
  margin-left: auto;
  display: flex;
  gap: 2px;
  padding-right: 6px;
  opacity: 0.25;
  transition: opacity 150ms ease;
  pointer-events: none;
}
.pleaf:hover .pleaf__actions,
.pleaf--focused .pleaf__actions {
  opacity: 1;
  pointer-events: auto;
}
.pleaf__action {
  border: 0;
  background: transparent;
  color: var(--text-dim, #8b8f98);
  cursor: pointer;
  font-size: 11px;
  padding: 2px 6px;
  border-radius: 4px;
}
.pleaf__action:hover {
  background: var(--bg-hover, #2c2f36);
  color: var(--text, #e6e6e6);
}
.pleaf__body {
  position: relative;
  flex: 1;
  min-height: 0;
  overflow: hidden;
}
.pleaf__empty {
  margin: auto;
  color: var(--text-dim, #8b8f98);
  font-size: 13px;
  padding: 24px;
  text-align: center;
}
</style>
