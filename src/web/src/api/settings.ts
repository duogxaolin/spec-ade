// REST calls for editor settings (SPEC-002 §3.1).
//
// The server exposes only the `editor` branch; `authToken` and `projects` are
// deliberately unreachable from here ([INVENTED-1]).

import { apiFetch } from './client';

/** Mirrors the server's `EditorSettings`. */
export interface EditorSettings {
  fontSize: number;
  tabSize: number;
  insertSpaces: boolean;
  wordWrap: boolean;
}

export interface SettingsView {
  editor: EditorSettings;
}

/**
 * Partial update. A key left out is kept; a key set to `null` goes back to the
 * server's default ([INVENTED-3]) — so `null` is meaningful and must survive
 * JSON.stringify, which is why these are `| null` rather than optional-only.
 */
export type EditorPatch = {
  [K in keyof EditorSettings]?: EditorSettings[K] | null;
};

export function getSettings(): Promise<SettingsView> {
  return apiFetch<SettingsView>('/api/settings');
}

export function putSettings(editor: EditorPatch): Promise<SettingsView> {
  return apiFetch<SettingsView>('/api/settings', {
    method: 'PUT',
    body: JSON.stringify({ editor }),
  });
}
