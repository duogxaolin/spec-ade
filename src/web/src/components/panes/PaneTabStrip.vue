// Tab strip of one pane leaf (SPEC-008 §5.6 tabs, §5.7 drag).
//
// Renders every tab of the leaf regardless of kind — file, terminal,
// session… — because after the pane system lands there are no separate
// "editor tabs": one strip per leaf, one visual language. Select and close
// delegate to the layout store (which routes file-tab closes through the
// scoped editor store's save-if-dirty handshake via PaneLeaf).
//
// Drag follows §3.7: the sensor arms at 12px of movement (below that it was a
// click), ALL leaf rects snapshot ONCE at drag start, zone resolution is pure
// (`panes/dropzone`), Escape cancels, and a half-pane preview shows where a
// drop would land. No-op guards from dropzone.resolveDrop plus the
// neighbour-same-side rule are enforced here where tree context lives.

<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue';

import { resolveDrop, resolveZone, type Point, type Rect } from '../../panes/dropzone';
import { findLeaf, leavesInOrder, type TabDescriptor } from '../../panes/tree';
import { useLayoutStore } from '../../stores/layout';

const props = defineProps<{
  leafId: string;
  tabs: TabDescriptor[];
  activeTabId: string | null;
}>();

const emit = defineEmits<{
  /** A tab was chosen for activation (click, not drag). */
  activate: [tabId: string];
  /** Close was requested; PaneLeaf handles dirty-file handshakes. */
  close: [tabId: string];
  /**
   * Drag outcome other than reorder-within-leaf: move `tabId` into
   * `toLeafId` at `toIndex` (tabstrip drop), or split toward `zone`.
   */
  drop: [
    payload:
      | { kind: 'move'; tabId: string; toLeafId: string; toIndex: number }
      | { kind: 'split'; tabId: string; zone: 'left' | 'right' | 'up' | 'down' },
  ];
}>();

const layout = useLayoutStore();

const DRAG_THRESHOLD_PX = 12;

// ---- non-reactive drag state (§3.7: no reactivity during a gesture) -------

let dragging: { tabId: string; startX: number; startY: number; armed: boolean } | null = null;
let rects = new Map<string, Rect>();
let hoverZone: ReturnType<typeof resolveZone> | null = null;
let hoverLeafId: string | null = null;

/** Reactive mirror only for the preview overlay + cursor, never the math. */
const armed = ref(false);
const pointerInside = ref(false);
const preview = ref<{ leafId: string; zone: ReturnType<typeof resolveZone> } | null>(null);

const rootEl = ref<HTMLElement | null>(null);

function leafRect(leafId: string): Rect | undefined {
  return rects.get(leafId);
}

function snapshotRects(): void {
  const next = new Map<string, Rect>();
  const ctx = layout.currentProjectId && layout.tree ? leavesInOrder(layout.tree) : [];
  for (const leaf of ctx) {
    const el = document.querySelector(`[data-pane-leaf="${leaf.id}"]`);
    if (el instanceof HTMLElement) {
      const r = el.getBoundingClientRect();
      next.set(leaf.id, { x: r.left, y: r.top, width: r.width, height: r.height });
    }
  }
  rects = next;
}

function onTabPointerDown(ev: PointerEvent, tabId: string): void {
  if (ev.button !== 0) return;
  dragging = { tabId, startX: ev.clientX, startY: ev.clientY, armed: false };
}

function onPointerMove(ev: PointerEvent): void {
  if (!dragging) return;
  if (!dragging.armed) {
    const dx = ev.clientX - dragging.startX;
    const dy = ev.clientY - dragging.startY;
    // Sensor arms past 12px — below that every event is still a click.
    if (Math.hypot(dx, dy) < DRAG_THRESHOLD_PX) return;
    dragging.armed = true;
    armed.value = true;
    snapshotRects();
  }

  const p: Point = { x: ev.clientX, y: ev.clientY };
  let hit: string | null = null;
  for (const [id, r] of rects) {
    if (p.x >= r.x && p.x <= r.x + r.width && p.y >= r.y && p.y <= r.y + r.height) {
      hit = id;
      break;
    }
  }

  pointerInside.value = hit !== null;
  if (hit === null) {
    preview.value = null;
    hoverZone = null;
    hoverLeafId = null;
    return;
  }

  const rect = rects.get(hit)!;
  const targetTabs = findLeaf(layout.tree ?? { kind: 'leaf', id: 'x', tabs: [], activeTabId: null }, hit)?.tabs.length ?? 0;
  const res = resolveDrop(rect, p, {
    sameLeaf: hit === props.leafId,
    targetTabCount: targetTabs,
  });

  // Neighbour-same-side no-op (§3.7): dragging my leaf's ONLY tab onto the
  // sibling produced by splitting me changes nothing — block it.
  let noop = res.noop;
  if (!noop && res.zone !== 'center' && res.zone !== 'tabstrip') {
    const sourceTabs = props.tabs.length;
    if (sourceTabs === 1 && hit === props.leafId) {
      // Splitting self with the last tab would auto-unsplit right back.
      noop = true;
    }
  }

  hoverLeafId = noop ? null : hit;
  hoverZone = noop ? null : res.zone;
  preview.value = noop || !res.zone || res.zone === 'tabstrip' || hit === null ? null : { leafId: hit, zone: res.zone };
}

function finishDrag(cancelled: boolean): void {
  const d = dragging;
  dragging = null;
  armed.value = false;
  pointerInside.value = false;
  const zoneSnapshot = hoverZone;
  const leafSnapshot = hoverLeafId;
  hoverZone = null;
  hoverLeafId = null;
  preview.value = null;
  rects = new Map();
  if (!d || cancelled || !d.armed) return;

  if (!cancelled && leafSnapshot && zoneSnapshot) {
    const tree = layout.tree;
    if (zoneSnapshot === 'tabstrip') {
      // Reorder/insert within the target's tab group.
      const targetTabs = tree ? (findLeaf(tree, leafSnapshot)?.tabs ?? []) : [];
      const toIndex =
        leafSnapshot === props.leafId
          ? Math.max(0, targetTabs.findIndex((t) => t.id === d.tabId))
          : targetTabs.length;
      emit('drop', { kind: 'move', tabId: d.tabId, toLeafId: leafSnapshot, toIndex });
    } else if (zoneSnapshot !== 'center') {
      emit('drop', { kind: 'split', tabId: d.tabId, zone: zoneSnapshot });
    }
  }
  void cancelled;
}

function onPointerUp(): void {
  finishDrag(false);
}
function onKeyDown(ev: KeyboardEvent): void {
  if (ev.key === 'Escape' && dragging?.armed) {
    ev.stopPropagation();
    finishDrag(true);
  }
}

onBeforeUnmount(() => {
  // Gesture dies with the component; nothing to commit — emits need a live parent.
  dragging = null;
  rects = new Map();
});

// ---- preview overlay geometry ---------------------------------------------

const previewStyle = computed(() => {
  if (!preview.value) return null;
  const rect = leafRect(preview.value.leafId);
  if (!rect) return null;
  const f = 0.5;
  switch (preview.value.zone) {
    case 'left':
      return { left: '0', top: '0', width: `${rect.width * f}px`, height: '100%' };
    case 'right':
      return { right: '0', top: '0', width: `${rect.width * f}px`, height: '100%' };
    case 'up':
      return { left: '0', top: '0', width: '100%', height: `${rect.height * f}px` };
    case 'down':
      return { left: '0', bottom: '0', width: '100%', height: `${rect.height * f}px` };
    default:
      return null;
  }
});

const titleOf = (t: TabDescriptor) => t.title;

defineExpose({ rootEl });
</script>

<template>
  <div
    ref="rootEl"
    class="pstrip"
    role="tablist"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @keydown="onKeyDown"
  >
    <button
      v-for="tab in tabs"
      :key="tab.id"
      class="pstrip__tab"
      :class="{ 'pstrip__tab--active': tab.id === activeTabId }"
      role="tab"
      :aria-selected="tab.id === activeTabId"
      :data-tab-kind="tab.kind"
      @pointerdown="onTabPointerDown($event, tab.id)"
      @click="emit('activate', tab.id)"
    >
      <span class="pstrip__title">{{ titleOf(tab) }}</span>
      <span
        class="pstrip__close"
        role="button"
        aria-label="Đóng tab"
        @pointerdown.stop
        @click.stop="emit('close', tab.id)"
        >×</span
      >
    </button>

    <div v-if="armed" class="pstrip__shield" aria-hidden="true" />
  </div>
</template>

<style scoped>
.pstrip {
  position: relative;
  display: flex;
  align-items: stretch;
  gap: 2px;
  height: var(--tabstrip-height, 32px);
  flex: none;
  overflow-x: auto;
  scrollbar-width: none;
  user-select: none;
}
.pstrip::-webkit-scrollbar {
  display: none;
}
.pstrip__tab {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 0 8px;
  max-width: 180px;
  border: 0;
  background: transparent;
  color: var(--text-dim, #8b8f98);
  font-size: 12px;
  cursor: pointer;
  white-space: nowrap;
}
.pstrip__tab--active {
  color: var(--text, #e6e6e6);
  background: var(--bg-raised, #23252b);
  box-shadow: inset 0 -2px 0 var(--accent, #4f8cff);
}
.pstrip__title {
  overflow: hidden;
  text-overflow: ellipsis;
}
.pstrip__close {
  border-radius: 3px;
  line-height: 1;
  padding: 1px 3px;
  visibility: hidden;
}
.pstrip__tab:hover .pstrip__close,
.pstrip__tab--active .pstrip__close {
  visibility: visible;
}
.pstrip__close:hover {
  background: var(--bg-hover, #2c2f36);
}
/* While armed, swallow pointer events over content so moves outside the
   strip keep updating the preview instead of clicking through. */
.pstrip__shield {
  position: fixed;
  inset: 0;
  z-index: 40;
  cursor: grabbing;
}
</style>
