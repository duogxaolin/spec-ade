<script setup lang="ts">
// CodeMirror 6 pane (SPEC-002 §5.7).
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

import { markRaw, onBeforeUnmount, onMounted, shallowRef, useTemplateRef, watch } from 'vue';
import { EditorState, Compartment, type Extension } from '@codemirror/state';
import { EditorView, keymap } from '@codemirror/view';
import { indentUnit } from '@codemirror/language';
import { basicSetup } from 'codemirror';
import { oneDark } from '@codemirror/theme-one-dark';

import { languageFor } from '../editor/languages';
import { useEditorStore } from '../stores/editor';
import { useSettingsStore } from '../stores/settings';

const props = defineProps<{ projectId: string }>();

const store = useEditorStore();
const settings = useSettingsStore();
const host = useTemplateRef<HTMLDivElement>('host');

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
 * A line to jump to once its document is mounted, from a search result (D39).
 *
 * Held rather than applied immediately because the click that asks for it may
 * arrive before the tab's content has been read — `open` is a round trip, and
 * `syncToActive` runs on a watcher, not synchronously.
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
 * Seed a freshly opened tab with content read from the server.
 *
 * Called by the parent with the `ReadResult` — the content goes straight into a
 * CM6 state and never passes through the store.
 */
function seed(path: string, content: string): void {
  const state = EditorState.create({ doc: content, extensions: baseExtensions(path) });
  states.set(path, state);
  if (store.activePath === path) mount(path, state);
}

/** Show whichever tab is active, if we have a document for it. */
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

  syncToActive();
});

onBeforeUnmount(() => {
  store.setContentProvider(null);
  view.value?.destroy();
  view.value = null;
  states.clear();
  mountedPath = null;
});

watch(() => store.activePath, syncToActive);

// Drop parked documents for tabs that were closed, so the map can't outgrow the
// tab strip.
watch(
  () => store.tabs.map((t) => t.path).join('\n'),
  () => {
    const open = new Set(store.tabs.map((t) => t.path));
    for (const path of [...states.keys()]) {
      if (!open.has(path)) states.delete(path);
    }
    if (mountedPath && !open.has(mountedPath)) mountedPath = null;
  },
);

watch(
  () => [
    settings.editor.fontSize,
    settings.editor.tabSize,
    settings.editor.insertSpaces,
    settings.editor.wordWrap,
  ],
  applyConfig,
);

defineExpose({ seed, reveal, focus: () => view.value?.focus() });
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
