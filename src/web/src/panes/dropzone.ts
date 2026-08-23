// Drop-zone geometry (SPEC-008 §3.7, [INVENTED-10]) — pure numbers, no DOM.
//
// Given a pane's rectangle and the pointer, decide what a drop means. The zones
// (§3.7 box):
//   - top 32px           → `tabstrip`  (reorder within the group, never split)
//   - within a 20% edge  → `left|right|up|down` (split, dragged tab to the new half)
//   - otherwise          → `center`   (merge the tab into the target group)
//
// The geometry snapshot is taken ONCE at dragStart by the caller (reading rects
// mid-drag causes reflow jank, §3.7); this module only maps rect+point → zone, so
// it is trivially unit-testable (F10–F13) with no Vue or layout involved.

export type DropZone = 'center' | 'left' | 'right' | 'up' | 'down' | 'tabstrip';

export interface Rect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface Point {
  x: number;
  y: number;
}

/** Top strip (px) immune to splitting — a drop here reorders tabs. */
export const TABSTRIP_HEIGHT = 32;
/** Fraction of each side that counts as a directional split band. */
export const EDGE_FRACTION = 0.2;

/**
 * Map a rect + pointer to a zone (F10–F12). The tab strip wins first; then the
 * NEAREST edge decides, but only if the pointer is within that edge's 20% band —
 * otherwise the drop is a center merge. Nearest-edge tie-break makes corners
 * deterministic (left/right beat up/down on an exact tie).
 */
export function resolveZone(rect: Rect, point: Point): DropZone {
  const relY = point.y - rect.y;
  if (relY <= TABSTRIP_HEIGHT) return 'tabstrip';

  const fx = rect.width > 0 ? (point.x - rect.x) / rect.width : 0.5;
  const fy = rect.height > 0 ? relY / rect.height : 0.5;

  const dist: Record<'left' | 'right' | 'up' | 'down', number> = {
    left: fx,
    right: 1 - fx,
    up: fy,
    down: 1 - fy,
  };
  let zone: 'left' | 'right' | 'up' | 'down' = 'left';
  let min = dist.left;
  if (dist.right < min) {
    min = dist.right;
    zone = 'right';
  }
  if (dist.up < min) {
    min = dist.up;
    zone = 'up';
  }
  if (dist.down < min) {
    min = dist.down;
    zone = 'down';
  }
  return min < EDGE_FRACTION ? zone : 'center';
}

export interface DropContext {
  /** Is the drop target the same leaf the drag started from? */
  sameLeaf: boolean;
  /** Tab count of the target leaf. */
  targetTabCount: number;
}

export interface DropResult {
  zone: DropZone;
  /** True → the caller must do nothing (a pointless self-drop). */
  noop: boolean;
}

/**
 * Resolve a full drop: the zone plus a no-op guard (F13). Dropping a tab back on
 * its own single-tab leaf can never change anything — center/tabstrip would
 * reorder a lone tab, an edge split would move the only tab out and then
 * auto-unsplit right back — so it is blocked outright. The neighbour-same-side
 * no-op (§3.7) needs tree context and is decided at the call site.
 */
export function resolveDrop(rect: Rect, point: Point, ctx: DropContext): DropResult {
  const zone = resolveZone(rect, point);
  const noop = ctx.sameLeaf && ctx.targetTabCount <= 1;
  return { zone, noop };
}
