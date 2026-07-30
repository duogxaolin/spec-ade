// Editor settings store (SPEC-002 §5.7).
//
// Holds the server's `editor` section. Defaults here mirror the server's
// ([INVENTED-2]) so the editor can mount before the first GET resolves; the
// server stays the source of truth and overwrites them on load.

import { defineStore } from 'pinia';
import { ref } from 'vue';

import {
  getSettings,
  putSettings,
  type EditorPatch,
  type EditorSettings,
} from '../api/settings';

const DEFAULTS: EditorSettings = {
  fontSize: 14,
  tabSize: 2,
  insertSpaces: true,
  wordWrap: false,
};

export const useSettingsStore = defineStore('settings', () => {
  const editor = ref<EditorSettings>({ ...DEFAULTS });
  const loaded = ref(false);
  const error = ref<string | null>(null);

  async function load(): Promise<void> {
    error.value = null;
    try {
      const view = await getSettings();
      editor.value = view.editor;
      loaded.value = true;
    } catch (err) {
      error.value = messageOf(err);
    }
  }

  /**
   * Send a partial update and adopt whatever the server returns.
   *
   * The response is applied instead of the local guess because the server
   * validates bounds and rejects out-of-range values (400, no clamp) — assuming
   * the patch landed would leave the UI showing a value that was never stored.
   */
  async function patch(changes: EditorPatch): Promise<boolean> {
    error.value = null;
    try {
      const view = await putSettings(changes);
      editor.value = view.editor;
      return true;
    } catch (err) {
      error.value = messageOf(err);
      return false;
    }
  }

  return { editor, loaded, error, load, patch };
});

function messageOf(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
