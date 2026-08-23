// Open-tab store (SPEC-002 §5.7).
//
// Holds tab METADATA only — `{path, rev, dirty, …}`. The CodeMirror `EditorState`
// for each tab lives in `EditorPane.vue`, outside reactive state: Pinia proxying
// CM6 internals is a documented crash/perf footgun (`07:40`, `04:39`), the same
// reason `stores/terminals.ts` keeps its xterm instance out of the store.
//
// Because the content is not in the store, saving needs a way to *ask* for it.
// That is `setContentProvider` — a plain function reference, deliberately not
// reactive, registered by the pane that owns the documents.

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import { conflictRev, readFile, writeFile, type ReadResult } from '../api/files';

/** One open tab. `rev` is the optimistic-concurrency tag from the last read/write. */
export interface Tab {
  /** Project-relative path — the tab's identity. */
  path: string;
  /** Basename, for the tab strip. */
  name: string;
  /** What the server said this file is; only `text` is editable. */
  kind: ReadResult['kind'];
  rev: string;
  size: number;
  dirty: boolean;
  /** Present for `binary`/`tooLarge`, to explain why it can't be opened. */
  mime?: string;
  eol?: 'lf' | 'crlf' | 'mixed';
}

/** A refused save, kept until the user picks overwrite or discard. */
export interface Conflict {
  path: string;
  /** The on-disk rev the server reported, i.e. what an overwrite would replace. */
  currentRev: string;
}

// The setup is one shared recipe; SPEC-008 (§5.8) keys it by `scope` so each
// `file` pane can own an independent tab set. `useEditorStore()` (below) resolves
// the recipe against a per-scope store id.
function editorSetup() {
  const tabs = ref<Tab[]>([]);
  const activePath = ref<string | null>(null);
  const error = ref<string | null>(null);
  const conflict = ref<Conflict | null>(null);

  /**
   * A pending "jump to line" request for a file pane (D39, SPEC-008 §5.8).
   *
   * Search hits and agent "open location" links must reveal a line in the pane
   * that shows the file. The click can arrive before the tab's content is read,
   * so the request is parked here and the pane applies it once mounted, then
   * clears it. Outside CM6 concerns — only `{path, line, nonce}`, so it stays
   * reactive safely. `nonce` makes a repeat reveal of the same line re-fire.
   */
  const reveal = ref<{ path: string; line: number; nonce: number } | null>(null);
  let revealNonce = 0;
  function requestReveal(path: string, line: number): void {
    revealNonce += 1;
    reveal.value = { path, line, nonce: revealNonce };
  }
  function clearReveal(): void {
    reveal.value = null;
  }

  /**
   * A pending "re-read this file from disk" request (SPEC-008 §5.9). The
   * conflict footer lives in App, but the document lives in a leaf's `EditorPane`
   * — App can't reach across the recursive tree to replace a CM6 state. So a
   * reload is parked here (same shape as `reveal`) and the owning pane, which
   * has the document, performs the read and swaps its state. `nonce` lets a
   * repeat reload of the same path re-fire.
   */
  const reloadRequest = ref<{ path: string; nonce: number } | null>(null);
  let reloadNonce = 0;
  function requestReload(path: string): void {
    reloadNonce += 1;
    reloadRequest.value = { path, nonce: reloadNonce };
  }
  function clearReload(): void {
    reloadRequest.value = null;
  }

  const activeTab = computed(
    () => tabs.value.find((t) => t.path === activePath.value) ?? null,
  );
  const hasDirty = computed(() => tabs.value.some((t) => t.dirty));

  /**
   * How `save` gets a tab's current text. Held outside `ref()` on purpose: it
   * closes over CM6 documents, which must not be made reactive.
   */
  let contentProvider: ((path: string) => string | null) | null = null;
  function setContentProvider(fn: ((path: string) => string | null) | null): void {
    contentProvider = fn;
  }

  function tabFor(path: string): Tab | undefined {
    return tabs.value.find((t) => t.path === path);
  }

  function patchTab(path: string, changes: Partial<Tab>): void {
    tabs.value = tabs.value.map((t) => (t.path === path ? { ...t, ...changes } : t));
  }

  function markDirty(path: string): void {
    const tab = tabFor(path);
    if (tab && !tab.dirty) patchTab(path, { dirty: true });
  }

  function tabFromRead(result: ReadResult): Tab {
    const name = result.path.split('/').pop() ?? result.path;
    return {
      path: result.path,
      name,
      kind: result.kind,
      rev: result.rev,
      size: result.size,
      dirty: false,
      mime: result.kind === 'text' ? undefined : result.mime,
      eol: result.kind === 'text' ? result.eol : undefined,
    };
  }

  /**
   * Open `path`, or focus it if already open.
   *
   * Returns the read result so the caller can seed a CM6 state with the content
   * — which never enters the store.
   */
  async function open(projectId: string, path: string): Promise<ReadResult | null> {
    error.value = null;

    if (tabFor(path)) {
      await activate(projectId, path);
      return null;
    }

    try {
      const result = await readFile(projectId, path);
      tabs.value = [...tabs.value, tabFromRead(result)];
      // Save the outgoing tab before switching away from it (`07:42`).
      await saveIfDirty(projectId, activePath.value);
      activePath.value = path;
      return result;
    } catch (err) {
      error.value = messageOf(err);
      return null;
    }
  }

  /** Switch tabs, auto-saving the one being left. */
  async function activate(projectId: string, path: string): Promise<void> {
    if (activePath.value === path) return;
    await saveIfDirty(projectId, activePath.value);
    if (tabFor(path)) activePath.value = path;
  }

  async function saveIfDirty(projectId: string, path: string | null): Promise<void> {
    if (!path) return;
    const tab = tabFor(path);
    if (tab?.dirty) await save(projectId, path);
  }

  /**
   * Persist one tab.
   *
   * `force` drops the `rev` precondition — the "Ghi đè" path, used only after a
   * 409 the user chose to resolve that way.
   */
  async function save(projectId: string, path: string, force = false): Promise<boolean> {
    const tab = tabFor(path);
    if (!tab || tab.kind !== 'text') return false;

    const content = contentProvider?.(path);
    if (content === null || content === undefined) {
      // No document for this tab (pane not mounted yet) — writing here would
      // send stale or empty content over a real file.
      return false;
    }

    error.value = null;
    try {
      const result = await writeFile(projectId, path, content, force ? undefined : tab.rev);
      patchTab(path, { rev: result.rev, size: result.size, dirty: false });
      if (conflict.value?.path === path) conflict.value = null;
      return true;
    } catch (err) {
      const currentRev = conflictRev(err);
      if (currentRev !== null) {
        // Stay dirty: the edit is still the user's only copy until they decide.
        conflict.value = { path, currentRev };
        error.value = `${path} đã bị sửa bên ngoài — chọn "Ghi đè" để lưu đè.`;
        return false;
      }
      error.value = messageOf(err);
      return false;
    }
  }

  /** Resolve a conflict by overwriting what's on disk. */
  async function overwrite(projectId: string, path: string): Promise<boolean> {
    return save(projectId, path, true);
  }

  function dismissConflict(): void {
    conflict.value = null;
  }

  /** Close a tab, saving it first if dirty (same rule as switching away). */
  async function close(projectId: string, path: string): Promise<void> {
    await saveIfDirty(projectId, path);
    tabs.value = tabs.value.filter((t) => t.path !== path);
    if (conflict.value?.path === path) conflict.value = null;
    if (activePath.value === path) {
      activePath.value = tabs.value[tabs.value.length - 1]?.path ?? null;
    }
  }

  /**
   * Drop every tab — used when switching project, where paths are relative to a
   * different root and would otherwise resolve against the wrong tree.
   */
  function reset(): void {
    tabs.value = [];
    activePath.value = null;
    conflict.value = null;
    error.value = null;
  }

  /** Forget a path after it was renamed or deleted outside the editor. */
  function forget(path: string): void {
    tabs.value = tabs.value.filter((t) => t.path !== path && !t.path.startsWith(`${path}/`));
    if (activePath.value && !tabs.value.some((t) => t.path === activePath.value)) {
      activePath.value = tabs.value[tabs.value.length - 1]?.path ?? null;
    }
  }

  return {
    tabs,
    activePath,
    activeTab,
    hasDirty,
    error,
    conflict,
    reveal,
    requestReveal,
    clearReveal,
    reloadRequest,
    requestReload,
    clearReload,
    setContentProvider,
    tabFor,
    open,
    activate,
    save,
    saveIfDirty,
    overwrite,
    dismissConflict,
    close,
    reset,
    forget,
    markDirty,
  };
}

/** Store definitions memoized per scope — Pinia keys stores by id, so one each. */
const editorStores = new Map<string, ReturnType<typeof makeEditorStore>>();

/**
 * Every scope ever resolved, as a REACTIVE list (SPEC-008 §5.9). App aggregates
 * conflicts/errors across all leaf-scoped editor stores through this: a computed
 * that reads `activeScopes` re-runs when a new leaf's store appears, so it then
 * starts tracking that store's `conflict`/`error` too. A bare `find` over a
 * private map could not — it would never re-evaluate when a later scope opened.
 */
const activeScopes = ref<string[]>([]);

function makeEditorStore(scope: string) {
  return defineStore(`editor:${scope}`, editorSetup);
}

/**
 * The open-tab store for `scope` — a pane leaf id, or `'default'` for the global
 * singleton. Backward compatible: `useEditorStore()` returns the same store it
 * always did (`editor:default`), so pre-pane callers and tests are unchanged
 * (SPEC-008 §5.8).
 */
export function useEditorStore(scope = 'default') {
  let use = editorStores.get(scope);
  if (!use) {
    use = makeEditorStore(scope);
    editorStores.set(scope, use);
    activeScopes.value = [...activeScopes.value, scope];
  }
  return use();
}

/** The reactive list of every editor scope resolved so far (§5.9 aggregation). */
export function useEditorScopes() {
  return activeScopes;
}

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
