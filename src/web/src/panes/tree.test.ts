import { describe, it, expect } from 'vitest';
import {
  makeLeaf,
  makeSplit,
  splitLeaf,
  setRatio,
  closeTab,
  moveTab,
  stripTabs,
  replaceNodeAtPath,
  updateLeaf,
  pathToLeaf,
  findLeaf,
  leavesInOrder,
  sanitize,
  isSplit,
  isLeaf,
  type PaneLeaf,
  type PaneSplit,
  type TabDescriptor,
} from './tree';
import { mruSelector } from './mru';

function tab(id: string, kind: TabDescriptor['kind'] = 'file'): TabDescriptor {
  return { id, kind, title: id, params: {} };
}

describe('splitLeaf (F1)', () => {
  it('wraps the leaf in a 0.5 split, new leaf on the requested side, old id kept', () => {
    const { tree, newLeafId } = splitLeaf(makeLeaf([tab('t1')], 'L'), 'L', 'horizontal', 'second');
    expect(isSplit(tree)).toBe(true);
    const s = tree as PaneSplit;
    expect(s.direction).toBe('horizontal');
    expect(s.ratio).toBe(0.5);
    expect((s.first as PaneLeaf).id).toBe('L'); // existing keeps its id
    expect((s.second as PaneLeaf).id).toBe(newLeafId); // new leaf on 'second'
  });

  it('puts the new leaf on first when side=first', () => {
    const { tree, newLeafId } = splitLeaf(makeLeaf([], 'L'), 'L', 'vertical', 'first');
    const s = tree as PaneSplit;
    expect((s.first as PaneLeaf).id).toBe(newLeafId);
    expect((s.second as PaneLeaf).id).toBe('L');
  });
});

describe('setRatio (F2)', () => {
  const base = makeSplit('horizontal', makeLeaf([], 'A'), makeLeaf([], 'B'), 0.5);
  it('clamps below the floor (0.05 → 0.15)', () => {
    expect((setRatio(base, [], 0.05) as PaneSplit).ratio).toBe(0.15);
  });
  it('clamps above the ceiling (0.95 → 0.85)', () => {
    expect((setRatio(base, [], 0.95) as PaneSplit).ratio).toBe(0.85);
  });
  it('keeps an in-band ratio and returns the same ref when unchanged', () => {
    expect(setRatio(base, [], 0.5)).toBe(base);
  });
});

describe('closeTab promote-sibling (F3)', () => {
  it('drops the parent split when a leaf empties, promoting the sibling', () => {
    const tree = makeSplit('horizontal', makeLeaf([tab('a')], 'A'), makeLeaf([tab('b')], 'B'));
    const res = closeTab(tree, 'A', 'a');
    expect(res.removed).toBe(true);
    expect(isLeaf(res.tree)).toBe(true); // split gone
    expect((res.tree as PaneLeaf).id).toBe('B'); // sibling promoted
    expect(res.focusLeafId).toBe('B');
  });
});

describe('closeTab root leaf (F4)', () => {
  it('keeps an emptied root leaf as a blank screen', () => {
    const res = closeTab(makeLeaf([tab('only')], 'R'), 'R', 'only');
    expect(res.removed).toBe(true);
    expect(isLeaf(res.tree)).toBe(true);
    const l = res.tree as PaneLeaf;
    expect(l.id).toBe('R');
    expect(l.tabs).toEqual([]);
    expect(l.activeTabId).toBeNull();
    expect(res.focusLeafId).toBe('R');
  });
});

describe('closeTab MRU next-active (F5)', () => {
  it('selects the most-recently-used survivor, not the left neighbour', () => {
    // Visual order t1..t4, active t2; MRU (most-recent-first) is t4 then t3.
    const leaf: PaneLeaf = { ...makeLeaf([tab('t1'), tab('t2'), tab('t3'), tab('t4')], 'L'), activeTabId: 't2' };
    const res = closeTab(leaf, 'L', 't2', mruSelector(['t4', 't3']));
    expect((res.tree as PaneLeaf).activeTabId).toBe('t4'); // NOT t1 (left) and NOT t3
  });
});

describe('moveTab (F6)', () => {
  it('cross-leaf move updates both source and destination', () => {
    const tree = makeSplit(
      'horizontal',
      makeLeaf([tab('a1'), tab('a2')], 'A'),
      makeLeaf([tab('b1')], 'B'),
    );
    const res = moveTab(tree, 'A', 'a1', 'B', 1);
    expect(res.moved).toBe(true);
    expect(findLeaf(res.tree, 'A')!.tabs.map((t) => t.id)).toEqual(['a2']); // source lost a1
    const B = findLeaf(res.tree, 'B')!;
    expect(B.tabs.map((t) => t.id)).toEqual(['b1', 'a1']); // dest gained a1 at index 1
    expect(B.activeTabId).toBe('a1'); // moved tab activated
    expect(res.focusLeafId).toBe('B');
  });

  it('moving the last tab out unsplits the source (F6 + F33)', () => {
    const tree = makeSplit('horizontal', makeLeaf([tab('a1')], 'A'), makeLeaf([tab('b1')], 'B'));
    const res = moveTab(tree, 'A', 'a1', 'B', 0);
    expect(isLeaf(res.tree)).toBe(true); // source emptied → promote
    const B = res.tree as PaneLeaf;
    expect(B.id).toBe('B');
    expect(B.tabs.map((t) => t.id)).toEqual(['a1', 'b1']);
  });

  it('same-leaf move is a pure reorder', () => {
    const res = moveTab(makeLeaf([tab('t1'), tab('t2'), tab('t3')], 'L'), 'L', 't1', 'L', 2);
    expect((res.tree as PaneLeaf).tabs.map((t) => t.id)).toEqual(['t2', 't3', 't1']);
  });
});

describe('structural sharing (F7)', () => {
  it('replaceNodeAtPath keeps untouched subtrees by reference', () => {
    const right = makeLeaf([tab('b')], 'B');
    const tree = makeSplit('horizontal', makeLeaf([tab('a')], 'A'), right);
    const updated = replaceNodeAtPath(tree, pathToLeaf(tree, 'A')!, makeLeaf([tab('a2')], 'A'));
    expect((updated as PaneSplit).second).toBe(right); // sibling shared by ref
    expect(updated).not.toBe(tree); // root rebuilt on the path
  });

  it('updateLeaf on an absent id returns the same tree ref', () => {
    const tree = makeSplit('horizontal', makeLeaf([], 'A'), makeLeaf([], 'B'));
    expect(updateLeaf(tree, 'ghost', (l) => l)).toBe(tree);
  });
});

describe('stripTabs (F9)', () => {
  it('keeps shape and ratios but empties every leaf', () => {
    const tree = makeSplit(
      'vertical',
      makeLeaf([tab('a')], 'A'),
      makeSplit('horizontal', makeLeaf([tab('b')], 'B'), makeLeaf([tab('c')], 'C'), 0.3),
      0.7,
    );
    const stripped = stripTabs(tree) as PaneSplit;
    expect(stripped.direction).toBe('vertical');
    expect(stripped.ratio).toBe(0.7);
    expect((stripped.first as PaneLeaf).tabs).toEqual([]);
    const inner = stripped.second as PaneSplit;
    expect(inner.ratio).toBe(0.3);
    expect((inner.first as PaneLeaf).tabs).toEqual([]);
    expect((inner.second as PaneLeaf).activeTabId).toBeNull();
  });
});

describe('leavesInOrder', () => {
  it('returns leaves left→right, top→bottom (cycle-focus order)', () => {
    const tree = makeSplit(
      'horizontal',
      makeLeaf([], 'A'),
      makeSplit('vertical', makeLeaf([], 'B'), makeLeaf([], 'C')),
    );
    expect(leavesInOrder(tree).map((l) => l.id)).toEqual(['A', 'B', 'C']);
  });
});

describe('sanitize (restore untrusted JSON)', () => {
  it('drops unknown tab kinds and malformed tabs, fixing a stale active id', () => {
    const node = sanitize({
      kind: 'leaf',
      id: 'L',
      tabs: [
        { id: 't1', kind: 'file', title: 'ok', params: {} },
        { id: 't2', kind: 'bogus', title: 'x', params: {} }, // unknown kind
        { kind: 'file', title: 'no id' }, // missing id
      ],
      activeTabId: 't2', // points at a dropped tab
    }) as PaneLeaf;
    expect(node.tabs.map((t) => t.id)).toEqual(['t1']);
    expect(node.activeTabId).toBe('t1'); // invalid active → last surviving
  });

  it('collapses a split with a dead child to its surviving side', () => {
    const node = sanitize({
      kind: 'split',
      direction: 'horizontal',
      ratio: 0.5,
      first: { kind: 'leaf', id: 'A', tabs: [], activeTabId: null },
      second: { kind: 'garbage' },
    }) as PaneLeaf;
    expect(isLeaf(node)).toBe(true);
    expect(node.id).toBe('A');
  });

  it('clamps a wild ratio and falls back to a blank leaf on total junk', () => {
    const wild = sanitize({
      kind: 'split',
      direction: 'vertical',
      ratio: 9,
      first: { kind: 'leaf', id: 'A', tabs: [], activeTabId: null },
      second: { kind: 'leaf', id: 'B', tabs: [], activeTabId: null },
    }) as PaneSplit;
    expect(wild.ratio).toBe(0.85);
    expect(isLeaf(sanitize(42))).toBe(true); // primitive → blank leaf
    expect(sanitize(null).kind).toBe('leaf');
  });
});


