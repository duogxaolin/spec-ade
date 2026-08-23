// Layout store (SPEC-008 §5.2) — the per-project pane tree and its focus, wired
// to the pure tree algebra in `panes/` and persisted through `api/layout`.
//
// State is keyed by project id: switching project shows a different tree while
// the others stay parked in `trees`. The tree objects themselves are IMMUTABLE
// (every `panes/tree` op returns a new root), so Vue reactivity fires on the
// `.set()` and no deep-watching is needed. Nothing here touches the DOM; the
// components read `tree`/`activeLeafId` and render.
//
// Persistence is DEBOUNCED (§3.3, [INVENTED-13]): split/resize bursts would
// otherwise write settings.json every frame. Because the server REPLACES the
// whole `projectLayouts` map on PUT, a save must send every project's tree — so
// `trees` is seeded from the full layout document on `load()` and stays the
// authoritative mirror.

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import { getLayout, putLayout, type LayoutPreset } from '../api/layout';
import { buildPreset, capturePreset, type PresetName } from '../panes/presets';
import { mruSelector, remove as mruRemove, touch as mruTouch } from '../panes/mru';
import {
  closeTab as treeCloseTab,
  findLeaf,
  firstLeaf,
  genId,
  leavesInOrder,
  makeLeaf,
  moveTab as treeMoveTab,
  sanitize,
  setRatio as treeSetRatio,
  splitLeaf,
  stripTabs,
  updateLeaf,
  type Direction,
  type PaneNode,
  type PanePath,
  type PaneSide,
  type TabDescriptor,
  type TabKind,
} from '../panes/tree';

/** Human label for a freshly opened singleton tab. */
function titleForKind(kind: TabKind): string {
  switch (kind) {
    case 'session':
      return 'Agent';
    case 'git':
      return 'Git';
    case 'search':
      return 'Tìm';
    case 'monitor':
      return 'Máy';
    case 'claws':
      return 'Claws';
    case 'terminal':
      return 'sh';
    default:
      return kind;
  }
}

/** Basename of a project-relative path, for a file tab's title. */
function baseName(path: string): string {
  return path.split('/').pop() || path;
}

/** Debounce before a layout mutation is flushed to the server (§3.3). */
export const SAVE_DEBOUNCE_MS = 500;

/** A brand-new project with no persisted layout starts as one empty pane (§5.9). */
function defaultTree(): PaneNode {
  return makeLeaf();
}

export const useLayoutStore = defineStore('layout', () => {
  /** Which project's tree is on screen; drives every computed accessor. */
  const currentProjectId = ref<string | null>(null);

  // Per-project maps. Reactive collections: `.get`/`.set` are tracked by Vue.
  const trees = ref<Map<string, PaneNode>>(new Map());
  const activeLeaf = ref<Map<string, string>>(new Map());
  const maximized = ref<Map<string, string | null>>(new Map());

  /** Per-leaf most-recent-first tab stack, for MRU next-active on close (§5.6). */
  const mru = ref<Map<string, string[]>>(new Map());

  /** Global, not per-project. */
  const presets = ref<LayoutPreset[]>([]);
  const lastLayout = ref<PaneNode | null>(null);

  const loaded = ref(false);
  const error = ref<string | null>(null);

  /**
   * Transient UI message from a pane (a terminal socket error, etc.). Panes are
   * rendered deep inside the tree, so they can't reach App's footer directly;
   * they drop a line here and App surfaces it in the notice chain (§5.9).
   */
  const notice = ref<string | null>(null);
  function setNotice(message: string): void {
    notice.value = message;
  }
  function clearNotice(): void {
    notice.value = null;
  }

  /**
   * Files a leaf tried to reopen on restore but couldn't (deleted since the
   * layout was saved, §5.9). Accumulated across panes so App can show ONE
   * aggregated "N file không mở lại được" notice instead of N separate ones.
   */
  const missingFiles = ref<string[]>([]);
  function noteMissingFile(path: string): void {
    if (!missingFiles.value.includes(path)) missingFiles.value = [...missingFiles.value, path];
  }
  function clearMissingFiles(): void {
    missingFiles.value = [];
  }
  const restoreNotice = computed<string | null>(() => {
    const n = missingFiles.value.length;
    if (n === 0) return null;
    const names = missingFiles.value.map((p) => p.split('/').pop() || p).join(', ');
    return `${n} file không mở lại được: ${names}`;
  });

  // ---- accessors for the current project -----------------------------------

  const tree = computed<PaneNode | null>(() => {
    const pid = currentProjectId.value;
    return pid ? trees.value.get(pid) ?? null : null;
  });
  const activeLeafId = computed<string | null>(() => {
    const pid = currentProjectId.value;
    return pid ? activeLeaf.value.get(pid) ?? null : null;
  });
  const maximizedLeafId = computed<string | null>(() => {
    const pid = currentProjectId.value;
    return pid ? maximized.value.get(pid) ?? null : null;
  });

  // ---- persistence (debounced) ---------------------------------------------

  // Not reactive — a bare timer handle, same rule the other stores use.
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Push every known project's tree in one PUT. The server replaces the whole
   * `projectLayouts` map, so a partial send would wipe the projects not included
   * — hence the full mirror. `lastLayout`/`layoutPresets` are authoritative here
   * too, so sending them back is safe and keeps the template current (§5.9).
   */
  async function flushSave(): Promise<void> {
    const projectLayouts: Record<string, PaneNode> = {};
    for (const [id, node] of trees.value.entries()) projectLayouts[id] = node;
    try {
      await putLayout({
        projectLayouts,
        lastLayout: lastLayout.value,
        layoutPresets: presets.value,
      });
    } catch (err) {
      error.value = messageOf(err);
    }
  }

  /**
   * Coalesce a burst of mutations into a single write (§3.3, [INVENTED-13]).
   * Each call resets the timer, so N changes inside the window flush once.
   */
  function scheduleSave(): void {
    if (saveTimer !== null) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      void flushSave();
    }, SAVE_DEBOUNCE_MS);
  }

  // ---- private mutation helpers --------------------------------------------

  /**
   * Adopt a new tree for `pid` and schedule a save. Mirrors the current tree
   * into `lastLayout` (tabs stripped) so a brand-new project inherits the shape.
   */
  function applyTree(pid: string, next: PaneNode): void {
    trees.value.set(pid, next);
    if (pid === currentProjectId.value) lastLayout.value = stripTabs(next);
    scheduleSave();
  }

  function touchMru(leafId: string, tabId: string): void {
    mru.value.set(leafId, mruTouch(mru.value.get(leafId) ?? [], tabId));
  }

  // ---- load / project switch -----------------------------------------------

  /**
   * Fetch the whole layout document once and seed the mirror. Trees are
   * SANITIZED on the way in (§5.9): the server stores them as opaque JSON, so a
   * hand-edited or older-schema tree must degrade to something renderable rather
   * than crash.
   */
  async function load(): Promise<void> {
    if (loaded.value) return;
    try {
      const view = await getLayout();
      for (const [id, node] of Object.entries(view.projectLayouts)) {
        trees.value.set(id, sanitize(node));
      }
      lastLayout.value = view.lastLayout ? sanitize(view.lastLayout) : null;
      presets.value = view.layoutPresets.map((p) => ({ name: p.name, tree: sanitize(p.tree) }));
      loaded.value = true;
    } catch (err) {
      error.value = messageOf(err);
    }
  }

  /**
   * Show `projectId`. A project with no tree yet inherits the `lastLayout`
   * template (empty leaves) or falls back to a single pane, and that fresh tree
   * is persisted. Focus always resets to the first leaf of the shown project
   * (F18); the previously shown project's tree is left untouched in `trees`.
   */
  function setProject(projectId: string): void {
    currentProjectId.value = projectId;
    let t = trees.value.get(projectId);
    if (!t) {
      t = lastLayout.value ? stripTabs(lastLayout.value) : defaultTree();
      trees.value.set(projectId, t);
      scheduleSave();
    }
    activeLeaf.value.set(projectId, firstLeaf(t).id);
    if (!maximized.value.has(projectId)) maximized.value.set(projectId, null);
  }

  // ---- tree actions (all operate on the current project) -------------------

  function requireCurrent(): { pid: string; t: PaneNode } | null {
    const pid = currentProjectId.value;
    if (!pid) return null;
    const t = trees.value.get(pid);
    return t ? { pid, t } : null;
  }

  /** Split the focused leaf and move focus onto the new one (F14). */
  function splitActive(direction: Direction, side: PaneSide = 'second'): void {
    const ctx = requireCurrent();
    if (!ctx) return;
    const leafId = activeLeaf.value.get(ctx.pid);
    if (!leafId) return;
    const { tree: next, newLeafId } = splitLeaf(ctx.t, leafId, direction, side);
    if (next === ctx.t) return;
    applyTree(ctx.pid, next);
    activeLeaf.value.set(ctx.pid, newLeafId);
  }

  /**
   * Split an ARBITRARY leaf (drop target of a tab drag, §3.7) with the old
   * leaf on `side`, focus the fresh half, and return its id — null when the
   * tree or the leaf vanished mid-gesture.
   */
  function splitLeafAt(
    leafId: string,
    direction: Direction,
    side: PaneSide,
  ): string | null {
    const ctx = requireCurrent();
    if (!ctx) return null;
    const { tree: next, newLeafId } = splitLeaf(ctx.t, leafId, direction, side);
    if (next === ctx.t) return null;
    applyTree(ctx.pid, next);
    activeLeaf.value.set(ctx.pid, newLeafId);
    return newLeafId;
  }

  /** Add a tab to the focused leaf and activate it. */
  function openTab(desc: TabDescriptor): void {
    const ctx = requireCurrent();
    if (!ctx) return;
    const leafId = activeLeaf.value.get(ctx.pid);
    if (!leafId) return;
    const next = updateLeaf(ctx.t, leafId, (leaf) => ({
      ...leaf,
      tabs: [...leaf.tabs, desc],
      activeTabId: desc.id,
    }));
    applyTree(ctx.pid, next);
    touchMru(leafId, desc.id);
  }

  /**
   * Open (or focus) the singleton tab of `kind` in the current project (F30,
   * §3.6). Singleton panels hold sockets/watchers, so a second open must find
   * the existing instance — focus its leaf and activate its tab — instead of
   * cloning it. Only when none exists is one created in the focused leaf.
   */
  function openSingleton(kind: TabKind): void {
    const ctx = requireCurrent();
    if (!ctx) return;
    for (const leaf of leavesInOrder(ctx.t)) {
      const tab = leaf.tabs.find((t) => t.kind === kind);
      if (tab) {
        if (leaf.id !== activeLeaf.value.get(ctx.pid)) focusLeaf(leaf.id);
        const next = updateLeaf(ctx.t, leaf.id, (l) => ({ ...l, activeTabId: tab.id }));
        if (next !== ctx.t) applyTree(ctx.pid, next);
        touchMru(leaf.id, tab.id);
        return;
      }
    }
    openTab({ id: genId('tab'), kind, title: titleForKind(kind), params: {} });
  }

  /**
   * Open (or focus) the file tab for `path` in the current project. De-dup key
   * is `params.path`, not the tab id — reopening an already-open file must
   * surface its existing tab wherever that pane lives. Returns true when a NEW
   * tab was created (the caller then seeds the CodeMirror state from the read).
   */
  function openFileTab(path: string): boolean {
    const ctx = requireCurrent();
    if (!ctx) return false;
    for (const leaf of leavesInOrder(ctx.t)) {
      const tab = leaf.tabs.find((t) => t.kind === 'file' && t.params.path === path);
      if (tab) {
        if (leaf.id !== activeLeaf.value.get(ctx.pid)) focusLeaf(leaf.id);
        const next = updateLeaf(ctx.t, leaf.id, (l) => ({ ...l, activeTabId: tab.id }));
        if (next !== ctx.t) applyTree(ctx.pid, next);
        touchMru(leaf.id, tab.id);
        return false;
      }
    }
    const desc: TabDescriptor = {
      id: genId('tab'),
      kind: 'file',
      title: baseName(path),
      params: { path },
    };
    openTab(desc);
    return true;
  }

  /** The file tab matching `path`, if any pane of this project holds it. */
  function fileTab(path: string): { leafId: string; tab: TabDescriptor } | null {
    const ctx = requireCurrent();
    if (!ctx) return null;
    for (const leaf of leavesInOrder(ctx.t)) {
      const tab = leaf.tabs.find((t) => t.kind === 'file' && t.params.path === path);
      if (tab) return { leafId: leaf.id, tab };
    }
    return null;
  }

  /** Make a tab active within its leaf, recording it at the top of the MRU. */
  function activateTab(leafId: string, tabId: string): void {
    const ctx = requireCurrent();
    if (!ctx) return;
    const next = updateLeaf(ctx.t, leafId, (leaf) =>
      leaf.tabs.some((tab) => tab.id === tabId) ? { ...leaf, activeTabId: tabId } : leaf,
    );
    if (next === ctx.t) return;
    applyTree(ctx.pid, next);
    touchMru(leafId, tabId);
  }

  /**
   * Close a tab, picking the next active by MRU (F5) and auto-unsplitting when
   * the leaf empties (F3). Focus follows the tree op's decision.
   */
  function closeTab(leafId: string, tabId: string): void {
    const ctx = requireCurrent();
    if (!ctx) return;
    const stack = mru.value.get(leafId) ?? [];
    const res = treeCloseTab(ctx.t, leafId, tabId, mruSelector(stack));
    if (!res.removed) return;
    applyTree(ctx.pid, res.tree);
    mru.value.set(leafId, mruRemove(stack, tabId));
    if (res.focusLeafId) activeLeaf.value.set(ctx.pid, res.focusLeafId);
  }

  /** Move a tab between (or within) leaves; focus follows the tab (F6, F33). */
  function moveTab(fromLeafId: string, tabId: string, toLeafId: string, toIndex: number): void {
    const ctx = requireCurrent();
    if (!ctx) return;
    const res = treeMoveTab(ctx.t, fromLeafId, tabId, toLeafId, toIndex);
    if (!res.moved) return;
    applyTree(ctx.pid, res.tree);
    if (res.focusLeafId) activeLeaf.value.set(ctx.pid, res.focusLeafId);
    touchMru(toLeafId, tabId);
  }

  /** Commit a split's ratio (F2). No-op when unchanged (tree op returns same ref). */
  function setRatio(path: PanePath, ratio: number): void {
    const ctx = requireCurrent();
    if (!ctx) return;
    const next = treeSetRatio(ctx.t, path, ratio);
    if (next === ctx.t) return;
    applyTree(ctx.pid, next);
  }

  function focusLeaf(leafId: string): void {
    const pid = currentProjectId.value;
    if (pid) activeLeaf.value.set(pid, leafId);
  }

  /** Stop showing any project (last one removed) without touching the mirror. */
  function clearCurrent(): void {
    currentProjectId.value = null;
  }

  /**
   * Forget a project's tree after it is deleted (F24 cascade, client side). The
   * server drops `projectLayouts[pid]` on project delete; the mirror must drop it
   * too, or the next debounced PUT — which sends the WHOLE map — would resurrect
   * it. Scheduling a save here makes the client and server agree.
   */
  function dropProject(pid: string): void {
    trees.value.delete(pid);
    activeLeaf.value.delete(pid);
    maximized.value.delete(pid);
    if (currentProjectId.value === pid) currentProjectId.value = null;
    scheduleSave();
  }

  /** Cycle focus through leaves in in-order, wrapping at the ends (F15). */
  function cycleFocus(delta: number): void {
    const ctx = requireCurrent();
    if (!ctx) return;
    const leaves = leavesInOrder(ctx.t);
    if (leaves.length === 0) return;
    const current = activeLeaf.value.get(ctx.pid);
    const idx = Math.max(0, leaves.findIndex((l) => l.id === current));
    const nextIdx = (idx + delta + leaves.length) % leaves.length;
    activeLeaf.value.set(ctx.pid, leaves[nextIdx].id);
  }

  /** Maximize the focused leaf, or restore if one is already maximized (F16). */
  function toggleMaximize(): void {
    const pid = currentProjectId.value;
    if (!pid) return;
    const current = maximized.value.get(pid) ?? null;
    maximized.value.set(pid, current ? null : activeLeaf.value.get(pid) ?? null);
  }

  /** Replace the current tree with a built-in preset's empty-leaf shape (§3.4). */
  function applyPreset(name: PresetName): void {
    const pid = currentProjectId.value;
    if (!pid) return;
    const next = buildPreset(name);
    applyTree(pid, next);
    activeLeaf.value.set(pid, firstLeaf(next).id);
    maximized.value.set(pid, null);
  }

  /** Capture the current tree's shape (tabs stripped) as a named preset (§3.4). */
  function savePreset(name: string): void {
    const ctx = requireCurrent();
    if (!ctx) return;
    presets.value = [
      ...presets.value.filter((p) => p.name !== name),
      { name, tree: capturePreset(ctx.t) },
    ];
    scheduleSave();
  }

  return {
    currentProjectId,
    trees,
    activeLeaf,
    maximized,
    presets,
    lastLayout,
    loaded,
    error,
    notice,
    setNotice,
    clearNotice,
    missingFiles,
    restoreNotice,
    noteMissingFile,
    clearMissingFiles,
    tree,
    activeLeafId,
    maximizedLeafId,
    load,
    setProject,
    splitActive,
    splitLeafAt,
    openTab,
    openSingleton,
    openFileTab,
    fileTab,
    activateTab,
    closeTab,
    moveTab,
    setRatio,
    focusLeaf,
    clearCurrent,
    dropProject,
    cycleFocus,
    toggleMaximize,
    applyPreset,
    savePreset,
  };
});

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

