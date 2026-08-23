// Unit tests for the layout store (SPEC-008 F14–F18).
//
// These pin the store's coordination logic — split/focus/maximize and the
// debounced persist — on top of the pure tree algebra it drives. `api/layout`
// is mocked so no HTTP happens; the 500ms debounce is exercised with fake
// timers, the same way the monitor store's poll fallback is.

import { beforeEach, afterEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

import { SAVE_DEBOUNCE_MS, useLayoutStore } from './layout';
import { firstLeaf, isLeaf, leavesInOrder, type PaneLeaf, type PaneSplit } from '../panes/tree';

const { getLayout, putLayout } = vi.hoisted(() => ({
  getLayout: vi.fn(),
  putLayout: vi.fn(),
}));

vi.mock('../api/layout', async () => {
  const actual = await vi.importActual<typeof import('../api/layout')>('../api/layout');
  return { ...actual, getLayout, putLayout };
});

const EMPTY = { projectLayouts: {}, lastLayout: null, layoutPresets: [] };

describe('layout store', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    vi.useFakeTimers();
    getLayout.mockReset().mockResolvedValue(EMPTY);
    putLayout.mockReset().mockResolvedValue(EMPTY);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('splitActive splits the current tree and focuses the new leaf (F14)', () => {
    const store = useLayoutStore();
    store.setProject('p1');
    const original = store.activeLeafId;
    expect(isLeaf(store.tree!)).toBe(true);

    store.splitActive('horizontal');

    const split = store.tree as PaneSplit;
    expect(split.kind).toBe('split');
    // Existing leaf keeps its id on `first`; focus moves to the new `second`.
    expect((split.first as PaneLeaf).id).toBe(original);
    expect(store.activeLeafId).toBe((split.second as PaneLeaf).id);
    expect(store.activeLeafId).not.toBe(original);
  });

  it('cycleFocus walks leaves in-order and wraps both ways (F15)', () => {
    const store = useLayoutStore();
    store.setProject('p1');
    store.splitActive('horizontal'); // A | B, focus B
    store.splitActive('horizontal'); // A | (B | C), focus C

    const ids = leavesInOrder(store.tree!).map((l) => l.id);
    expect(ids).toHaveLength(3);
    expect(store.activeLeafId).toBe(ids[2]);

    store.cycleFocus(1); // C → wrap to first
    expect(store.activeLeafId).toBe(ids[0]);
    store.cycleFocus(1);
    expect(store.activeLeafId).toBe(ids[1]);

    store.focusLeaf(ids[0]);
    store.cycleFocus(-1); // first → wrap to last
    expect(store.activeLeafId).toBe(ids[2]);
  });

  it('toggleMaximize sets then clears the current project maximize (F16)', () => {
    const store = useLayoutStore();
    store.setProject('p1');
    expect(store.maximizedLeafId).toBeNull();

    store.toggleMaximize();
    expect(store.maximizedLeafId).toBe(store.activeLeafId);

    store.toggleMaximize();
    expect(store.maximizedLeafId).toBeNull();
  });

  it('coalesces a burst of mutations into one debounced putLayout (F17)', async () => {
    const store = useLayoutStore();
    store.setProject('p1');
    // Flush the save that opening the project scheduled, then start clean.
    await vi.advanceTimersByTimeAsync(SAVE_DEBOUNCE_MS);
    putLayout.mockClear();

    store.splitActive('horizontal');
    store.splitActive('horizontal');
    // Still inside the debounce window: nothing written yet.
    expect(putLayout).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(SAVE_DEBOUNCE_MS);
    expect(putLayout).toHaveBeenCalledTimes(1);
    // The full mirror is sent — the changed project's tree is a split now.
    const body = putLayout.mock.calls[0][0];
    expect(body.projectLayouts.p1.kind).toBe('split');
  });

  it('switching project resets focus but keeps the old tree (F18)', () => {
    const store = useLayoutStore();
    store.setProject('p1');
    store.splitActive('horizontal'); // p1 now a split, focus on the new leaf
    const p1Tree = store.tree;
    expect(store.activeLeafId).not.toBe(firstLeaf(p1Tree!).id); // focus is on `second`

    store.setProject('p2');
    expect(store.currentProjectId).toBe('p2');
    expect(store.activeLeafId).toBe(firstLeaf(store.tree!).id); // new project → first leaf
    expect(store.trees.get('p1')).toBe(p1Tree); // old tree parked, untouched

    store.setProject('p1');
    expect(store.tree).toBe(p1Tree); // same tree still there
    expect(store.activeLeafId).toBe(firstLeaf(p1Tree!).id); // focus reset to first leaf
  });
});
