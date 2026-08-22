// Claws store — the persisted definitions plus their live runtime status
// (SPEC-007 §5.8).
//
// One rule with a test-shaped consequence: **status is server-owned**. The UI
// never infers `running` from having clicked Start — it re-reads what the server
// reports, because the runtime can move a claw to `error` (spawn failure, keepAlive
// exhausted) without any request from this tab. Every mutation therefore adopts
// the returned row instead of patching local state.

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import {
  createClaw,
  deleteClaw,
  listClaws,
  startClaw,
  stopClaw,
  updateClaw,
  type ClawInput,
  type ClawRow,
} from '../api/claws';

export const useClawsStore = defineStore('claws', () => {
  const claws = ref<ClawRow[]>([]);
  /** Skills of the project currently being browsed by the form. */
  const skills = ref<Record<string, { name: string; source: string; description?: string | null }[]>>({});
  const loading = ref(false);
  const busyId = ref<string | null>(null);
  const error = ref<string | null>(null);

  /** Claws of one project, definition order preserved (the server's). */
  function forProject(projectId: string): ClawRow[] {
    return claws.value.filter((c) => c.projectId === projectId);
  }

  const hasAny = computed(() => claws.value.length > 0);

  async function refresh(projectId?: string): Promise<void> {
    loading.value = true;
    error.value = null;
    try {
      const rows = await listClaws(projectId);
      if (projectId) {
        // Merge so another project's claws survive a filtered refresh.
        const others = claws.value.filter((c) => c.projectId !== projectId);
        claws.value = [...others, ...rows];
      } else {
        claws.value = rows;
      }
    } catch (err) {
      error.value = messageOf(err);
    } finally {
      loading.value = false;
    }
  }

  /**
   * Replace the stored row for `id` with the server's answer.
   *
   * A missing row is appended rather than ignored: between this client's last
   * refresh and now, another tab could have created it.
   */
  function adopt(row: ClawRow): void {
    const index = claws.value.findIndex((c) => c.id === row.id);
    if (index === -1) {
      claws.value = [...claws.value, row];
    } else {
      claws.value = claws.value.map((c) => (c.id === row.id ? row : c));
    }
  }

  async function add(input: ClawInput): Promise<ClawRow | null> {
    error.value = null;
    try {
      const row = await createClaw(input);
      adopt(row);
      return row;
    } catch (err) {
      error.value = messageOf(err);
      return null;
    }
  }

  async function save(id: string, input: ClawInput): Promise<ClawRow | null> {
    error.value = null;
    try {
      const row = await updateClaw(id, input);
      adopt(row);
      return row;
    } catch (err) {
      error.value = messageOf(err);
      return null;
    }
  }

  async function remove(id: string): Promise<boolean> {
    error.value = null;
    try {
      await deleteClaw(id);
      claws.value = claws.value.filter((c) => c.id !== id);
      return true;
    } catch (err) {
      error.value = messageOf(err);
      return false;
    }
  }

  /** Bring the connection up now. The response carries the fresh status. */
  async function start(id: string): Promise<boolean> {
    busyId.value = id;
    error.value = null;
    try {
      const { status } = await startClaw(id);
      applyStatus(id, status);
      return true;
    } catch (err) {
      error.value = messageOf(err);
      // A failed start still moved the runtime (state = error); re-read so the
      // row shows it instead of the pre-click state.
      await refreshOne(id);
      return false;
    } finally {
      busyId.value = null;
    }
  }

  /** Stop — idempotent on the server (E20), so no local "already stopped" check. */
  async function stop(id: string): Promise<boolean> {
    busyId.value = id;
    error.value = null;
    try {
      const { status } = await stopClaw(id);
      applyStatus(id, status);
      return true;
    } catch (err) {
      error.value = messageOf(err);
      await refreshOne(id);
      return false;
    } finally {
      busyId.value = null;
    }
  }

  function applyStatus(id: string, status: ClawRow['status']): void {
    claws.value = claws.value.map((c) => (c.id === id ? { ...c, status } : c));
  }

  /** Single-row re-read; a 404 here means it was deleted elsewhere — drop it. */
  async function refreshOne(id: string): Promise<void> {
    try {
      const { getClaw } = await import('../api/claws');
      adopt(await getClaw(id));
    } catch (err) {
      if (isNotFound(err)) claws.value = claws.value.filter((c) => c.id !== id);
    }
  }

  /** Cache the skill list per project — the form re-requests when its select opens. */
  async function loadSkills(projectId: string): Promise<{ name: string; source: string }[]> {
    const { listSkills } = await import('../api/claws');
    try {
      const found = await listSkills(projectId);
      skills.value = {
        ...skills.value,
        [projectId]: found.map(({ name, source, description }) => ({ name, source, description })),
      };
      return found;
    } catch (err) {
      error.value = messageOf(err);
      return [];
    }
  }

  function reset(): void {
    claws.value = [];
    skills.value = {};
    error.value = null;
    loading.value = false;
    busyId.value = null;
  }

  return {
    claws,
    skills,
    loading,
    busyId,
    error,
    hasAny,
    forProject,
    refresh,
    add,
    save,
    remove,
    start,
    stop,
    loadSkills,
    reset,
  };
});

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function isNotFound(err: unknown): boolean {
  return err instanceof Error && 'status' in err && (err as { status: number }).status === 404;
}
