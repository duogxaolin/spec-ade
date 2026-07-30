// Project registry store — which projects exist and which one the sidebar shows.
//
// Metadata only; the file tree's own cache lives in `FileTree.vue` so a project
// switch can drop it wholesale.

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import {
  createProject,
  deleteProject,
  duplicateProjectId,
  listProjects,
  updateProject,
  type CreateProjectRequest,
  type Project,
  type UpdateProjectRequest,
} from '../api/projects';

/** sessionStorage key for the selected project, so a reload stays put. */
const ACTIVE_KEY = 'spec_ade_active_project';

export const useProjectsStore = defineStore('projects', () => {
  const projects = ref<Project[]>([]);
  const activeId = ref<string | null>(sessionStorage.getItem(ACTIVE_KEY));
  const loading = ref(false);
  const error = ref<string | null>(null);

  const active = computed(
    () => projects.value.find((p) => p.id === activeId.value) ?? null,
  );

  function select(id: string | null): void {
    activeId.value = id;
    if (id) sessionStorage.setItem(ACTIVE_KEY, id);
    else sessionStorage.removeItem(ACTIVE_KEY);
  }

  /** Load the registry. The server already returns it in display order (§3.2). */
  async function refresh(): Promise<void> {
    loading.value = true;
    error.value = null;
    try {
      projects.value = await listProjects();
      // A remembered id that no longer exists must not leave the sidebar
      // pointing at nothing.
      if (!projects.value.some((p) => p.id === activeId.value)) {
        select(projects.value[0]?.id ?? null);
      }
    } catch (err) {
      error.value = messageOf(err);
    } finally {
      loading.value = false;
    }
  }

  /**
   * Register a project and select it.
   *
   * A 409 means the path is already registered; the server tells us which id
   * that is, so we select the existing project instead of reporting a dead end.
   */
  async function add(request: CreateProjectRequest): Promise<Project | null> {
    error.value = null;
    try {
      const project = await createProject(request);
      projects.value = [...projects.value, project];
      select(project.id);
      return project;
    } catch (err) {
      const existing = duplicateProjectId(err);
      if (existing) {
        select(existing);
        error.value = 'Thư mục này đã được thêm — đã chuyển sang project đó.';
        return projects.value.find((p) => p.id === existing) ?? null;
      }
      error.value = messageOf(err);
      return null;
    }
  }

  async function patch(id: string, body: UpdateProjectRequest): Promise<void> {
    error.value = null;
    try {
      const updated = await updateProject(id, body);
      projects.value = projects.value.map((p) => (p.id === id ? updated : p));
    } catch (err) {
      error.value = messageOf(err);
    }
  }

  async function remove(id: string): Promise<void> {
    error.value = null;
    try {
      await deleteProject(id);
    } catch (err) {
      // Report, then still drop it locally: a 404 means it's already gone.
      error.value = messageOf(err);
    }
    projects.value = projects.value.filter((p) => p.id !== id);
    if (activeId.value === id) {
      select(projects.value[0]?.id ?? null);
    }
  }

  return {
    projects,
    activeId,
    active,
    loading,
    error,
    refresh,
    add,
    patch,
    remove,
    select,
  };
});

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
