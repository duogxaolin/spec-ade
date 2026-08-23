// Pure pane-tree algebra (SPEC-008 §3.1, §5.6) — no Vue, no DOM.
//
// The tree is BINARY on purpose ([INVENTED-1]): a split has exactly two children
// and one `ratio`, so every operation here (split, promote-sibling, resize) has a
// single obvious shape. A 3-way split is just nested binary splits — visually
// identical, no special case. Keeping this module free of the framework is what
// lets the whole grammar be unit-tested the way `search/group.ts` is.
//
// Everything is IMMUTABLE and shares structure: a transform rebuilds only the
// nodes on the path from the root to the change, and returns every untouched
// subtree by reference (F7). That is both cheap and exactly what Vue's reactivity
// wants — a new root object signals "something changed" without deep-diffing.

export type Direction = 'horizontal' | 'vertical';

export type TabKind =
  | 'file'
  | 'terminal'
  | 'session'
  | 'git'
  | 'search'
  | 'monitor'
  | 'claws';

/** One tab: a view kind plus the minimum params needed to rebuild it (§3.2). */
export interface TabDescriptor {
  id: string;
  kind: TabKind;
  title: string;
  params: Record<string, unknown>;
}

export interface PaneLeaf {
  kind: 'leaf';
  id: string;
  tabs: TabDescriptor[];
  activeTabId: string | null;
}

export interface PaneSplit {
  kind: 'split';
  direction: Direction;
  /** Belongs to `first`; clamped to [RATIO_MIN, RATIO_MAX]. */
  ratio: number;
  first: PaneNode;
  second: PaneNode;
}

export type PaneNode = PaneLeaf | PaneSplit;

/** Which half of a split; also the steps of a path from the root. */
export type PaneSide = 'first' | 'second';
export type PanePath = PaneSide[];

export const RATIO_MIN = 0.15;
export const RATIO_MAX = 0.85;
export const RATIO_DEFAULT = 0.5;

/** Clamp a raw ratio into the allowed band so no pane can be resized to nothing. */
export function clampRatio(ratio: number): number {
  if (!Number.isFinite(ratio)) return RATIO_DEFAULT;
  return Math.min(RATIO_MAX, Math.max(RATIO_MIN, ratio));
}

let idCounter = 0;

/**
 * A stable id for a leaf or tab. Uses `crypto.randomUUID` when present (browser,
 * modern Node) and a monotonic fallback otherwise so unit tests never depend on
 * the crypto global being polyfilled.
 */
export function genId(prefix = 'pane'): string {
  const c: Crypto | undefined = globalThis.crypto;
  if (c && typeof c.randomUUID === 'function') return c.randomUUID();
  idCounter += 1;
  return `${prefix}-${Date.now().toString(36)}-${idCounter}`;
}

export function makeLeaf(tabs: TabDescriptor[] = [], id: string = genId('leaf')): PaneLeaf {
  return {
    kind: 'leaf',
    id,
    tabs,
    activeTabId: tabs.length ? tabs[tabs.length - 1].id : null,
  };
}

export function makeSplit(
  direction: Direction,
  first: PaneNode,
  second: PaneNode,
  ratio: number = RATIO_DEFAULT,
): PaneSplit {
  return { kind: 'split', direction, ratio: clampRatio(ratio), first, second };
}

export const isLeaf = (node: PaneNode): node is PaneLeaf => node.kind === 'leaf';
export const isSplit = (node: PaneNode): node is PaneSplit => node.kind === 'split';

// ---- navigation (read-only) ------------------------------------------------

/** The node at `path`, or null if the path runs off a leaf. */
export function nodeAtPath(tree: PaneNode, path: PanePath): PaneNode | null {
  let node: PaneNode = tree;
  for (const step of path) {
    if (node.kind !== 'split') return null;
    node = node[step];
  }
  return node;
}

/** The path from root to the leaf with `leafId`, or null. `[]` = the root leaf. */
export function pathToLeaf(tree: PaneNode, leafId: string): PanePath | null {
  if (tree.kind === 'leaf') return tree.id === leafId ? [] : null;
  const inFirst = pathToLeaf(tree.first, leafId);
  if (inFirst) return ['first', ...inFirst];
  const inSecond = pathToLeaf(tree.second, leafId);
  if (inSecond) return ['second', ...inSecond];
  return null;
}

export function findLeaf(tree: PaneNode, leafId: string): PaneLeaf | null {
  const path = pathToLeaf(tree, leafId);
  return path ? (nodeAtPath(tree, path) as PaneLeaf) : null;
}

/** Every leaf in in-order (left→right, top→bottom) — the cycle-focus order (§5.5). */
export function leavesInOrder(tree: PaneNode): PaneLeaf[] {
  if (tree.kind === 'leaf') return [tree];
  return [...leavesInOrder(tree.first), ...leavesInOrder(tree.second)];
}

/** The leftmost (first-descending) leaf of a subtree — where focus falls after promote. */
export function firstLeaf(node: PaneNode): PaneLeaf {
  let n = node;
  while (n.kind === 'split') n = n.first;
  return n;
}

// ---- transforms (immutable, structure-sharing) -----------------------------

/**
 * Return a tree with the node at `path` replaced. Untouched subtrees are shared
 * by reference; only the ancestors on `path` are rebuilt (F7). An empty path
 * replaces the whole tree.
 */
export function replaceNodeAtPath(
  tree: PaneNode,
  path: PanePath,
  replacement: PaneNode,
): PaneNode {
  if (path.length === 0) return replacement;
  if (tree.kind !== 'split') return tree; // path invalid — leave unchanged
  const [head, ...rest] = path;
  const child = tree[head];
  const newChild = replaceNodeAtPath(child, rest, replacement);
  if (newChild === child) return tree;
  return { ...tree, [head]: newChild };
}

/** Replace a leaf (found by id) with the result of `updater`. No-op if absent. */
export function updateLeaf(
  tree: PaneNode,
  leafId: string,
  updater: (leaf: PaneLeaf) => PaneLeaf,
): PaneNode {
  const path = pathToLeaf(tree, leafId);
  if (!path) return tree;
  const leaf = nodeAtPath(tree, path) as PaneLeaf;
  return replaceNodeAtPath(tree, path, updater(leaf));
}

/**
 * Split a leaf in two. The existing leaf (id preserved) stays on the opposite
 * side; `side` says where the NEW leaf lands. Returns the new tree and the new
 * leaf's id so the caller can focus it or drop a tab into it.
 */
export function splitLeaf(
  tree: PaneNode,
  leafId: string,
  direction: Direction,
  side: PaneSide,
  newLeaf: PaneLeaf = makeLeaf(),
): { tree: PaneNode; newLeafId: string } {
  const path = pathToLeaf(tree, leafId);
  if (!path) return { tree, newLeafId: leafId };
  const existing = nodeAtPath(tree, path) as PaneLeaf;
  const split =
    side === 'first'
      ? makeSplit(direction, newLeaf, existing)
      : makeSplit(direction, existing, newLeaf);
  return { tree: replaceNodeAtPath(tree, path, split), newLeafId: newLeaf.id };
}

/** Result of a structural change: the new tree plus where focus should land. */
export interface TreeChange {
  tree: PaneNode;
  focusLeafId: string | null;
}

/**
 * Replace the parent split of the leaf at `emptyLeafPath` with its sibling
 * subtree (auto-unsplit, §5.6). Focus falls to the sibling's first leaf. A root
 * leaf has no parent, so it survives as a blank screen.
 */
function promoteSibling(tree: PaneNode, emptyLeafPath: PanePath): TreeChange {
  if (emptyLeafPath.length === 0) {
    return { tree, focusLeafId: tree.kind === 'leaf' ? tree.id : null };
  }
  const parentPath = emptyLeafPath.slice(0, -1);
  const side = emptyLeafPath[emptyLeafPath.length - 1];
  const parent = nodeAtPath(tree, parentPath) as PaneSplit;
  const sibling = side === 'first' ? parent.second : parent.first;
  return {
    tree: replaceNodeAtPath(tree, parentPath, sibling),
    focusLeafId: firstLeaf(sibling).id,
  };
}

/** Default next-active pick when the store passes no MRU selector: the last tab. */
function lastTabId(tabs: TabDescriptor[]): string | null {
  return tabs.length ? tabs[tabs.length - 1].id : null;
}

export interface CloseResult extends TreeChange {
  removed: boolean;
}

/**
 * Remove a tab from a leaf. If the closed tab was active, `selectNextActive`
 * chooses the replacement (the store passes an MRU-based picker for F5). If the
 * leaf empties, its parent split is promoted away (F3); a root leaf stays blank
 * (F4).
 */
export function closeTab(
  tree: PaneNode,
  leafId: string,
  tabId: string,
  selectNextActive: (surviving: TabDescriptor[]) => string | null = lastTabId,
): CloseResult {
  const path = pathToLeaf(tree, leafId);
  if (!path) return { tree, removed: false, focusLeafId: null };
  const leaf = nodeAtPath(tree, path) as PaneLeaf;
  if (!leaf.tabs.some((t) => t.id === tabId)) {
    return { tree, removed: false, focusLeafId: leaf.id };
  }

  const tabs = leaf.tabs.filter((t) => t.id !== tabId);
  if (tabs.length === 0) {
    if (path.length === 0) {
      const blank: PaneLeaf = { ...leaf, tabs: [], activeTabId: null };
      return { tree: blank, removed: true, focusLeafId: blank.id };
    }
    const promoted = promoteSibling(tree, path);
    return { ...promoted, removed: true };
  }

  const activeTabId =
    leaf.activeTabId === tabId ? selectNextActive(tabs) : leaf.activeTabId;
  const newLeaf: PaneLeaf = { ...leaf, tabs, activeTabId };
  return { tree: replaceNodeAtPath(tree, path, newLeaf), removed: true, focusLeafId: leaf.id };
}

/**
 * Set the ratio of the split at `path`, clamped to the allowed band (F2). No-op
 * (same-ref return) if the path is not a split or the clamped value is unchanged
 * — the store relies on ref equality to skip a redundant persist.
 */
export function setRatio(tree: PaneNode, path: PanePath, ratio: number): PaneNode {
  const node = nodeAtPath(tree, path);
  if (!node || node.kind !== 'split') return tree;
  const clamped = clampRatio(ratio);
  if (clamped === node.ratio) return tree;
  return replaceNodeAtPath(tree, path, { ...node, ratio: clamped });
}

/**
 * Return a structural clone with every leaf emptied (F9). Splits, directions and
 * ratios are preserved — this is how a layout PRESET is captured from a live
 * tree: the shape without the content.
 */
export function stripTabs(node: PaneNode): PaneNode {
  if (node.kind === 'leaf') {
    return { ...node, tabs: [], activeTabId: null };
  }
  return { ...node, first: stripTabs(node.first), second: stripTabs(node.second) };
}

export interface MoveResult extends TreeChange {
  moved: boolean;
}

/**
 * Move a tab (F6). Same leaf → pure reorder at `toIndex`, focus unchanged. Cross
 * leaf → remove from the source (promoting its parent away if it empties, F3),
 * then insert into the target at `toIndex` and make it active. The target path is
 * recomputed AFTER the source removal: a promote can restructure the tree and
 * shift where the target leaf lives.
 */
export function moveTab(
  tree: PaneNode,
  fromLeafId: string,
  tabId: string,
  toLeafId: string,
  toIndex: number,
): MoveResult {
  const fromPath = pathToLeaf(tree, fromLeafId);
  if (!fromPath) return { tree, moved: false, focusLeafId: null };
  const fromLeaf = nodeAtPath(tree, fromPath) as PaneLeaf;
  const tab = fromLeaf.tabs.find((t) => t.id === tabId);
  if (!tab) return { tree, moved: false, focusLeafId: fromLeaf.id };

  if (fromLeafId === toLeafId) {
    const without = fromLeaf.tabs.filter((t) => t.id !== tabId);
    const index = Math.max(0, Math.min(toIndex, without.length));
    const tabs = [...without.slice(0, index), tab, ...without.slice(index)];
    return {
      tree: replaceNodeAtPath(tree, fromPath, { ...fromLeaf, tabs }),
      moved: true,
      focusLeafId: fromLeaf.id,
    };
  }

  const surviving = fromLeaf.tabs.filter((t) => t.id !== tabId);
  let working: PaneNode;
  if (surviving.length === 0 && fromPath.length > 0) {
    working = promoteSibling(tree, fromPath).tree;
  } else {
    const activeTabId =
      fromLeaf.activeTabId === tabId ? lastTabId(surviving) : fromLeaf.activeTabId;
    working = replaceNodeAtPath(tree, fromPath, { ...fromLeaf, tabs: surviving, activeTabId });
  }

  const toPath = pathToLeaf(working, toLeafId);
  if (!toPath) return { tree: working, moved: false, focusLeafId: toLeafId };
  const toLeaf = nodeAtPath(working, toPath) as PaneLeaf;
  const index = Math.max(0, Math.min(toIndex, toLeaf.tabs.length));
  const tabs = [...toLeaf.tabs.slice(0, index), tab, ...toLeaf.tabs.slice(index)];
  const merged: PaneLeaf = { ...toLeaf, tabs, activeTabId: tab.id };
  return {
    tree: replaceNodeAtPath(working, toPath, merged),
    moved: true,
    focusLeafId: toLeaf.id,
  };
}

// ---- sanitize (restore an opaque persisted tree) ---------------------------
//
// The server stores trees as OPAQUE JSON (§3.3), so on restore the shape is
// UNTRUSTED: an older client, a hand-edited settings.json, or a future schema
// could all hand us junk. `sanitize` is total — it always returns a usable tree,
// never throws — so a corrupt layout degrades to a blank leaf instead of a white
// screen. Unknown tab kinds and malformed tabs are dropped; a split missing a
// child collapses to its surviving side; ratios are clamped.

const TAB_KINDS: readonly TabKind[] = [
  'file',
  'terminal',
  'session',
  'git',
  'search',
  'monitor',
  'claws',
];

function sanitizeTab(tab: unknown): TabDescriptor | null {
  if (!tab || typeof tab !== 'object') return null;
  const t = tab as Record<string, unknown>;
  if (typeof t.id !== 'string' || !t.id) return null;
  if (typeof t.kind !== 'string' || !(TAB_KINDS as readonly string[]).includes(t.kind)) {
    return null;
  }
  const title = typeof t.title === 'string' ? t.title : '';
  const params =
    t.params && typeof t.params === 'object' ? (t.params as Record<string, unknown>) : {};
  return { id: t.id, kind: t.kind as TabKind, title, params };
}

function sanitizeLeaf(n: Record<string, unknown>): PaneLeaf {
  const rawTabs = Array.isArray(n.tabs) ? n.tabs : [];
  const tabs = rawTabs.map(sanitizeTab).filter((t): t is TabDescriptor => t !== null);
  const id = typeof n.id === 'string' && n.id ? n.id : genId('leaf');
  const activeValid =
    typeof n.activeTabId === 'string' && tabs.some((t) => t.id === n.activeTabId);
  const activeTabId = activeValid
    ? (n.activeTabId as string)
    : tabs.length
      ? tabs[tabs.length - 1].id
      : null;
  return { kind: 'leaf', id, tabs, activeTabId };
}

function sanitizeNode(node: unknown): PaneNode | null {
  if (!node || typeof node !== 'object') return null;
  const n = node as Record<string, unknown>;
  if (n.kind === 'leaf') return sanitizeLeaf(n);
  if (n.kind === 'split') {
    const first = sanitizeNode(n.first);
    const second = sanitizeNode(n.second);
    if (first && second) {
      const direction: Direction = n.direction === 'vertical' ? 'vertical' : 'horizontal';
      const ratio = typeof n.ratio === 'number' ? n.ratio : RATIO_DEFAULT;
      return makeSplit(direction, first, second, ratio);
    }
    // A split with a dead side collapses to whichever child survived.
    return first ?? second ?? null;
  }
  return null;
}

/** Total restore of an untrusted persisted node — never throws, always usable. */
export function sanitize(node: unknown): PaneNode {
  return sanitizeNode(node) ?? makeLeaf();
}


