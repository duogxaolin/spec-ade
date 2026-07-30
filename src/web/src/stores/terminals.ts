// Terminal registry store — which shells exist and which one is on screen.
//
// Holds only metadata. The `xterm.js` instance and its WebSocket live in
// `TerminalPane.vue`, deliberately outside reactive state: wrapping them in a
// Vue proxy breaks their internals (the same reason CodeMirror needs a
// `shallowRef` in Pha 2).

import { defineStore } from 'pinia';
import { ref } from 'vue';

import {
  killTerminal,
  listTerminals,
  spawnTerminal,
  type SpawnRequest,
  type TerminalInfo,
} from '../api/terminals';

export const useTerminalsStore = defineStore('terminals', () => {
  const terminals = ref<TerminalInfo[]>([]);
  const activeId = ref<string | null>(null);
  const loading = ref(false);
  const error = ref<string | null>(null);

  /**
   * Load what the server already has.
   *
   * Called on mount: shells outlive the page (SPEC-001 §4 [INVENTED-4]), so a
   * reload must re-adopt them rather than spawn duplicates.
   */
  async function refresh(): Promise<void> {
    loading.value = true;
    error.value = null;
    try {
      terminals.value = await listTerminals();
      const stillThere = terminals.value.some((t) => t.id === activeId.value);
      if (!stillThere) {
        activeId.value = terminals.value[0]?.id ?? null;
      }
    } catch (err) {
      error.value = messageOf(err);
    } finally {
      loading.value = false;
    }
  }

  async function create(request: SpawnRequest = {}): Promise<TerminalInfo | null> {
    error.value = null;
    try {
      const info = await spawnTerminal(request);
      terminals.value = [...terminals.value, info];
      activeId.value = info.id;
      return info;
    } catch (err) {
      error.value = messageOf(err);
      return null;
    }
  }

  async function destroy(id: string): Promise<void> {
    error.value = null;
    try {
      await killTerminal(id);
    } catch (err) {
      // Report, then still drop it locally: a 404 means it's already gone, and
      // leaving a dead tab on screen is worse than a stale error.
      error.value = messageOf(err);
    }
    terminals.value = terminals.value.filter((t) => t.id !== id);
    if (activeId.value === id) {
      activeId.value = terminals.value[0]?.id ?? null;
    }
  }

  function select(id: string): void {
    activeId.value = id;
  }

  /** Record a shell's exit without removing the tab, so output stays readable. */
  function markExited(id: string, exitCode: number | null): void {
    terminals.value = terminals.value.map((t) =>
      t.id === id ? { ...t, alive: false, exitCode } : t,
    );
  }

  function updateCwd(id: string, cwd: string): void {
    terminals.value = terminals.value.map((t) => (t.id === id ? { ...t, cwd } : t));
  }

  return {
    terminals,
    activeId,
    loading,
    error,
    refresh,
    create,
    destroy,
    select,
    markExited,
    updateCwd,
  };
});

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
