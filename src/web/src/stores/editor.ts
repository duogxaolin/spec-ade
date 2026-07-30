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

export const useEditorStore = defineStore('editor', () => {
  const tabs = ref<Tab[]>([]);
  const activePath = ref<string | null>(null);
  const error = ref<string | null>(null);
  const conflict = ref<Conflict | null>(null);

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
    setContentProvider,
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
});

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
