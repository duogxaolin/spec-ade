// Open a file into the focused pane (SPEC-008 §5.8) — the one place that turns
// a "show me this path" intent into a file tab plus an optional line reveal.
//
// Three callers need identical behaviour: the sidebar FileTree (@open), the
// SearchPanel (@open with a line), and AcpPane (@open-location with a line).
// Routing them through one helper keeps the de-dup rule (F30 for files: reopen
// focuses the existing tab) and the reveal handshake in a single place.
//
// The reveal is decoupled from the pane instance on purpose: `openFileTab` puts
// the tab in the focused leaf; we then drop the line request onto that leaf's
// SCOPED editor store. The `EditorPane` for that leaf reads the file content
// itself (driven by the tab appearing in its `paths`) and applies the parked
// reveal once its document mounts — no ref-reaching across the recursive tree.

import { useEditorStore } from '../stores/editor';
import { useLayoutStore } from '../stores/layout';

/**
 * Open `path` in the current project's focused pane, optionally revealing
 * `line` (1-based). A file already open anywhere in the tree is focused rather
 * than duplicated. Fire-and-forget: the target `EditorPane` reconciles its own
 * content from the layout tree, so this only mutates the tree and parks the
 * reveal request.
 */
export function useOpenFile() {
  const layout = useLayoutStore();

  return function openFile(projectId: string | null, path: string, line?: number | null): void {
    if (!projectId) return;
    layout.openFileTab(path);
    if (line == null) return;
    const leafId = layout.activeLeafId;
    if (leafId) useEditorStore(leafId).requestReveal(path, line);
  };
}
