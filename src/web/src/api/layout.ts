// REST calls for pane layout persistence (SPEC-008 §3.3).
//
// The server treats every pane tree as OPAQUE JSON: it stores and returns the
// nodes verbatim and never parses the grammar (leaf/split/ratio/tabs). So the
// types here are just the frontend's `PaneNode` shape passed straight through —
// the contract the server enforces is size (≤256 KiB) and registered project
// keys, not structure.

import { apiFetch } from './client';
import type { PaneNode } from '../panes/tree';

/** One saved layout preset: a name plus a tab-stripped tree (§3.4). */
export interface LayoutPreset {
  name: string;
  tree: PaneNode;
}

/** Mirrors the server's `LayoutView` — the whole layout document. */
export interface LayoutView {
  /** Per-project current tree, keyed by project id. */
  projectLayouts: Record<string, PaneNode>;
  /** Template for brand-new projects (tabs stripped), or null. */
  lastLayout: PaneNode | null;
  /** Global reusable presets. */
  layoutPresets: LayoutPreset[];
}

/**
 * Partial update. A top-level key left out is kept by the server; sending one
 * replaces it. `lastLayout` is a double-option server-side: absent (undefined,
 * dropped by JSON.stringify) keeps it, `null` clears it, a value sets it — which
 * is why it is `| null` rather than optional-only, exactly like `EditorPatch`.
 */
export interface LayoutPatch {
  projectLayouts?: Record<string, PaneNode>;
  lastLayout?: PaneNode | null;
  layoutPresets?: LayoutPreset[];
}

export function getLayout(): Promise<LayoutView> {
  return apiFetch<LayoutView>('/api/layout');
}

export function putLayout(patch: LayoutPatch): Promise<LayoutView> {
  return apiFetch<LayoutView>('/api/layout', {
    method: 'PUT',
    body: JSON.stringify(patch),
  });
}
