// Built-in layout presets (SPEC-008 §3.4, [INVENTED-12]) — pure tree builders.
//
// The product docs (`pane-system.mdx:54-58`) give three preset NAMES — Single,
// Side by side, Grid — but no shapes. We fix the trees here (F8). Applying a
// preset replaces the current project tree with EMPTY leaves for the user to
// fill; saving a custom preset captures a live tree's shape with tabs stripped.

import { makeLeaf, makeSplit, stripTabs, type PaneNode } from './tree';

export type PresetName = 'Single' | 'Side by side' | 'Grid';

/** The three built-ins, in menu order. */
export const BUILTIN_PRESETS: readonly PresetName[] = ['Single', 'Side by side', 'Grid'];

/**
 * Build a preset tree with empty leaves. Grid is a 2×2 as nested binary splits
 * (`split{h,.5, split{v,.5,leaf,leaf}, split{v,.5,leaf,leaf}}`, F8) — there is no
 * N-ary node ([INVENTED-1]), so a quad is two vertical splits under a horizontal.
 */
export function buildPreset(name: PresetName): PaneNode {
  switch (name) {
    case 'Single':
      return makeLeaf();
    case 'Side by side':
      return makeSplit('horizontal', makeLeaf(), makeLeaf(), 0.5);
    case 'Grid':
      return makeSplit(
        'horizontal',
        makeSplit('vertical', makeLeaf(), makeLeaf(), 0.5),
        makeSplit('vertical', makeLeaf(), makeLeaf(), 0.5),
        0.5,
      );
  }
}

/**
 * Capture a live tree as a reusable preset: shape + ratios preserved, every tab
 * stripped (§3.4). The result is a `PaneNode` ready to store under a name in
 * `layoutPresets`.
 */
export function capturePreset(tree: PaneNode): PaneNode {
  return stripTabs(tree);
}
