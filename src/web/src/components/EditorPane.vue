<script setup lang="ts">
// CodeMirror 6 pane (SPEC-002 §5.7, re-scoped for panes in SPEC-008 §5.8).
//
// Hard constraints, all of them from the docs rather than taste:
//
// - ONE `EditorView` for every tab; switching tabs is `view.setState(...)`
//   ([INVENTED-16]). Rebuilding the view per tab would throw away cursor and
//   undo history and churn the DOM.
// - The view lives in a `shallowRef` and is `markRaw`-ed (`07:40`, `04:39`).
//   A deep Vue proxy over CM6 internals is a documented crash/perf footgun.
// - Per-tab `EditorState`s live in a plain `Map`, NOT in reactive state, for the
//   same reason. The store only ever sees `{path, rev, dirty}`.
// - Settings changes go through `Compartment.reconfigure`, never a rebuild
//   (`07:41`).
// - State is only ever changed by dispatching transactions (`04:39`).
//
// SPEC-008 shift: this pane is now SELF-RECONCILING and SCOPED. It owns one
// leaf's file tabs via `useEditorStore(scope=leafId)` and derives which files
// to show from the LAYOUT tree — the parent no longer calls `seed`/`reveal`.
// When its leaf's active file tab has no CM document yet, the pane reads it
// itself; a file that no longer exists is dropped and reported for App's
// aggregated restore notice (§5.9). Line reveals arrive through the scoped
// store's `reveal` slot, parked until the document mounts (D39).

import { computed, markRaw, onBeforeUnmount, onMounted, shallowRef, useTemplateRef, watch } from 'vue';
import { EditorState, Compartment, type Extension } from '@codemirror/state';
import { EditorView, keymap } from '@codemirror/view';
import { indentUnit } from '@codemirror/language';
import { basicSetup } from 'codemirror';
import { oneDark } from '@codemirror/theme-one-dark';

import { languageFor } from '../editor/languages';
import { findLeaf, type PaneLeaf } from '../panes/tree';
import { useEditorStore } from '../stores/editor';
import { useLayoutStore } from '../stores/layout';
import { useSettingsStore } from '../stores/settings';

const props = defineProps<{
  projectId: string;
  /** Leaf id this pane belongs to; keys its scoped tab set. Defaults global. */
  scope?: string;
}>();

const store = useEditorStore(props.scope ?? 'default');
const layout = useLayoutStore();
const settings = useSettingsStore();
const host = useTemplateRef<HTMLDivElement>('host');

/** This pane's leaf in the current tree, or null if it vanished. */
const leaf = computed<PaneLeaf | null>(() => {
  if (!props.scope) return null;
  const t = layout.tree;
  return t ? findLeaf(t, props.scope) : null;
});

/** Project-relative paths of the leaf's file tabs, in strip order. */
const fileTabPaths = computed<string[]>(() => {
  const l = leaf.value;
  if (!l) return [];
  return l.tabs
    .filter((t) => t.kind === 'file' && typeof t.params.path === 'string')
    .map((t) => t.params.path as string);
});

/** The leaf's active tab path IF it is a file tab; else null. */
const activeFilePath = computed<string | null>(() => {
  const l = leaf.value;
  if (!l) return null;
  const tab = l.tabs.find((t) => t.id === l.activeTabId);
  return tab && tab.kind === 'file' && typeof tab.params.path === 'string'
    ? (tab.params.path as string)
    : null;
});

// `shallowRef`: the ref cell is reactive, the CM6 object inside it is not.
const view = shallowRef<EditorView | null>(null);

/**
 * One `EditorState` per open tab, deliberately outside reactive state.
 *
 * The active tab's live document is in `view.state`, not here — this map holds
 * the *parked* states, plus the active one as of the last switch.
 */
const states = new Map<string, EditorState>();
/** Which path the view currently holds; the key for dirty marking and saving. */
let mountedPath: string | null = null;

// Compartments so a settings change reconfigures in place (`07:41`).
const languageConf = new Compartment();
const tabConf = new Compartment();
const wrapConf = new Compartment();
const fontConf = new Compartment();

function tabExtensions(): Extension {
  const size = settings.editor.tabSize;
  return [
    EditorState.tabSize.of(size),
    indentUnit.of(settings.editor.insertSpaces ? ' '.repeat(size) : '\t'),
  ];
}

function fontTheme(): Extension {
  const px = `${settings.editor.fontSize}px`;
  return EditorView.theme({
    '&': { fontSize: px },
    '.cm-content': { fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace' },
  });
}

/** Extensions shared by every tab's state. */
function baseExtensions(path: string): Extension[] {
  return [
    basicSetup,
    oneDark,
    languageConf.of(languageFor(path)),
    tabConf.of(tabExtensions()),
    wrapConf.of(settings.editor.wordWrap ? EditorView.lineWrapping : []),
    fontConf.of(fontTheme()),
    // Cmd/Ctrl+S saves the active tab (§5.7). Returns true so the browser's own
    // save dialog never opens.
    keymap.of([
      {
        key: 'Mod-s',
        preventDefault: true,
        run: () => {
          if (mountedPath) void store.save(props.projectId, mountedPath);
          return true;
        },
      },
    ]),
    EditorView.updateListener.of((update) => {
      if (update.docChanged && mountedPath) store.markDirty(mountedPath);
    }),
    EditorView.contentAttributes.of({ 'aria-label': `Nội dung ${path}` }),
  ];
}

/**
 * Push the current settings onto whatever state the view holds.
 *
 * Needed after every `setState`: a compartment's value belongs to the state, so
 * a state parked before a settings change still carries the old configuration.
 */
function applyConfig(): void {
  const v = view.value;
  if (!v) return;
  v.dispatch({
    effects: [
      tabConf.reconfigure(tabExtensions()),
      wrapConf.reconfigure(settings.editor.wordWrap ? EditorView.lineWrapping : []),
      fontConf.reconfigure(fontTheme()),
    ],
  });
}

/** Park the live document so switching away and back keeps cursor + undo. */
function parkCurrent(): void {
  const v = view.value;
  if (v && mountedPath) states.set(mountedPath, v.state);
}

function mount(path: string, state: EditorState): void {
  const v = view.value;
  if (!v) return;
  parkCurrent();
  mountedPath = path;
  v.setState(state);
  applyConfig();
  flushPendingReveal();
}

/**
 * A line to jump to once its document is mounted (D39).
 *
 * Held rather than applied immediately because the click that asks for it may
 * arrive before the tab's content has been read — loading is a round trip, and
 * reconciliation runs on a watcher, not synchronously. The scoped store's
 * `reveal` slot feeds this; `mount` flushes it.
 */
let pendingReveal: { path: string; line: number } | null = null;

function flushPendingReveal(): void {
  const target = pendingReveal;
  const v = view.value;
  if (!target || !v || target.path !== mountedPath) return;
  pendingReveal = null;
  // The server's line numbers are 1-based and can point past a file that shrank
  // between the search and the click.
  const number = Math.min(Math.max(target.line, 1), v.state.doc.lines);
  const line = v.state.doc.line(number);
  v.dispatch({
    selection: { anchor: line.from },
    effects: EditorView.scrollIntoView(line.from, { y: 'center' }),
  });
  v.focus();
}

/** Scroll to and select the start of `line` in `path` (1-based). */
function reveal(path: string, line: number): void {
  pendingReveal = { path, line };
  flushPendingReveal();
}

/**
 * Seed a freshly read tab with content.
 *
 * The content goes straight into a CM6 state and never passes through the
 * store. If it is the active tab, mount it now.
 */
function seed(path: string, content: string): void {
  const state = EditorState.create({ doc: content, extensions: baseExtensions(path) });
  states.set(path, state);
  if (store.activePath === path) mount(path, state);
}

/**
 * Make the scoped editor store hold the file at `path`, reading it if this is
 * the first time. A read failure (file deleted since the layout was saved)
 * drops the tab from the leaf and records it for the aggregated notice (§5.9).
 */
async function ensureLoaded(path: string): Promise<void> {
  if (!props.projectId) return;
  // Already in the scoped store → text is seeded, or it's a binary/tooLarge
  // tab whose message the template already renders. Nothing to read.
  if (store.tabFor(path)) return;

  const result = await store.open(props.projectId, path);
  if (result) {
    if (result.kind === 'text') seed(result.path, result.content);
    return;
  }
  // open() returned null WITHOUT the tab existing → the read failed. Drop the
  // dead tab from this leaf and report it.
  const l = leaf.value;
  const dead = l?.tabs.find((t) => t.kind === 'file' && t.params.path === path);
  if (l && dead) {
    layout.noteMissingFile(path);
    layout.closeTab(l.id, dead.id);
  }
}

/** Show whichever file tab the leaf marks active, loading it on demand. */
async function reconcileActive(): Promise<void> {
  const path = activeFilePath.value;
  if (!path) return;
  await ensureLoaded(path);
  // open() already set the store's activePath on a fresh read; for an
  // already-loaded tab, align the store so `mount` picks it up.
  if (store.activePath !== path && store.tabFor(path)) {
    await store.activate(props.projectId, path);
  }
  syncToActive();
}

/** Mount the store's active tab if we hold a document for it. */
function syncToActive(): void {
  const path = store.activePath;
  if (!path || path === mountedPath) return;
  const state = states.get(path);
  if (state) mount(path, state);
}

onMounted(() => {
  if (!host.value) return;

  // `markRaw` before the object can be reached by anything reactive.
  view.value = markRaw(
    new EditorView({
      state: EditorState.create({ doc: '', extensions: baseExtensions('') }),
      parent: host.value,
    }),
  );

  // The store asks the pane for text at save time: documents live here.
  store.setContentProvider((path) => {
    const v = view.value;
    if (v && path === mountedPath) return v.state.doc.toString();
    return states.get(path)?.doc.toString() ?? null;
  });

  void reconcileActive();
});

onBeforeUnmount(() => {
  store.setContentProvider(null);
  view.value?.destroy();
  view.value = null;
  states.clear();
  mountedPath = null;
});

// Leaf's active file tab changed (strip click, drop, open) → load + mount it.
watch(activeFilePath, () => void reconcileActive());

// A parked reveal from the scoped store (search hit, agent link, F-tree open).
watch(
  () => store.reveal,
  (r) => {
    if (!r) return;
    reveal(r.path, r.line);
    store.clearReveal();
  },
  { deep: true },
);

// A parked "reload from disk" (conflict → "Nạp lại từ đĩa", §5.9). Only this
// pane holds the file's CM6 document, so it does the re-read and re-seed; App
// just parks the request on the owning leaf's scoped store.
watch(
  () => store.reloadRequest,
  async (r) => {
    if (!r) return;
    const path = r.path;
    store.clearReload();
    // Drop the stale tab so `open` re-reads from disk instead of just focusing.
    store.forget(path);
    states.delete(path);
    if (mountedPath === path) mountedPath = null;
    const result = await store.open(props.projectId, path);
    if (result && result.kind === 'text') seed(result.path, result.content);
  },
  { deep: true },
);

// Drop parked documents for file tabs that left this leaf, so the map can't
// outgrow the strip. Driven by the LEAF's file tabs, not the store's, so a
// tab moved to another pane is released here.
watch(fileTabPaths, (paths) => {
  const open = new Set(paths);
  for (const path of [...states.keys()]) {
    if (!open.has(path)) states.delete(path);
  }
  for (const tab of [...store.tabs]) {
    if (!open.has(tab.path)) store.forget(tab.path);
  }
  if (mountedPath && !open.has(mountedPath)) mountedPath = null;
});

watch(
  () => [
    settings.editor.fontSize,
    settings.editor.tabSize,
    settings.editor.insertSpaces,
    settings.editor.wordWrap,
  ],
  applyConfig,
);

defineExpose({ focus: () => view.value?.focus() });
</script>

<template>
  <div class="pane">
    <!-- The host stays mounted even for a binary tab: destroying and rebuilding
         the view is exactly what [INVENTED-16] rules out. -->
    <div v-show="store.activeTab?.kind === 'text'" ref="host" class="pane__cm" />

    <p v-if="!store.activeTab" class="pane__empty">Chọn một file để mở.</p>
    <p v-else-if="store.activeTab.kind === 'binary'" class="pane__empty">
      {{ store.activeTab.mime }} · {{ store.activeTab.size }} bytes — file nhị phân, không mở được
      trong editor.
    </p>
    <p v-else-if="store.activeTab.kind === 'tooLarge'" class="pane__empty">
      {{ store.activeTab.mime }} · {{ store.activeTab.size }} bytes — file quá lớn để mở trong
      editor.
    </p>
  </div>
</template>

<style scoped>
.pane {
  display: flex;
  flex: 1;
  min-height: 0;
  flex-direction: column;
}
.pane__cm {
  flex: 1;
  min-height: 0;
  overflow: hidden;
}
/* CM6 needs an explicit height to scroll rather than grow unbounded. */
.pane__cm :deep(.cm-editor) {
  height: 100%;
}
.pane__empty {
  padding: 1.5rem;
  color: #9e9e9e;
  font-size: 13px;
}
</style>
