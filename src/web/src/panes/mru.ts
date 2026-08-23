// Per-leaf MRU (most-recently-used) tab stack (SPEC-008 §5.6, [INVENTED-11]) —
// pure, no Vue. When the active tab of a leaf is closed, the replacement is the
// most-recently-used SURVIVING tab, not the left neighbour (F5). We keep a
// `recentTabIds` stack per leaf for exactly this, as a plain string[] the layout
// store threads through `closeTab`'s selector hook.
//
// Invariant: the stack is most-recent-FIRST, deduplicated. It may name tabs that
// no longer exist (a race between close and touch); `pickNext` tolerates that by
// intersecting with the surviving set rather than trusting the stack blindly.

import type { TabDescriptor } from './tree';

/** Most-recent-first, deduplicated list of tab ids. */
export type MruStack = string[];

/** Move `id` to the front (most recent). Idempotent on order for a repeat touch. */
export function touch(stack: MruStack, id: string): MruStack {
  return [id, ...stack.filter((x) => x !== id)];
}

/** Drop `id` from the stack. No-op if absent. */
export function remove(stack: MruStack, id: string): MruStack {
  return stack.filter((x) => x !== id);
}

/**
 * Pick the next-active tab after a close (§5.6.3): the top-of-stack entry that is
 * still alive, else the last surviving tab, else null. `surviving` is the leaf's
 * tabs AFTER the closed one was removed, so a stale id for the closed tab is
 * skipped automatically — callers need not `remove` first.
 */
export function pickNext(stack: MruStack, surviving: TabDescriptor[]): string | null {
  const alive = new Set(surviving.map((t) => t.id));
  for (const id of stack) {
    if (alive.has(id)) return id;
  }
  return surviving.length ? surviving[surviving.length - 1].id : null;
}

/**
 * A `closeTab`-compatible selector bound to a stack: `closeTab(tree, leaf, tab,
 * mruSelector(stack))` selects the MRU survivor. Keeps `tree.ts` ignorant of the
 * MRU policy while letting the store inject it.
 */
export function mruSelector(stack: MruStack): (surviving: TabDescriptor[]) => string | null {
  return (surviving) => pickNext(stack, surviving);
}
