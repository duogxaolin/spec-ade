import { describe, it, expect } from 'vitest';
import { buildPreset, capturePreset, BUILTIN_PRESETS } from './presets';
import { makeLeaf, makeSplit, isLeaf, type PaneLeaf, type PaneSplit, type TabDescriptor } from './tree';

function tab(id: string): TabDescriptor {
  return { id, kind: 'file', title: id, params: {} };
}

describe('buildPreset', () => {
  it('Single → a lone empty leaf', () => {
    const p = buildPreset('Single');
    expect(isLeaf(p)).toBe(true);
    expect((p as PaneLeaf).tabs).toEqual([]);
  });

  it('Side by side → one horizontal split of two empty leaves', () => {
    const p = buildPreset('Side by side') as PaneSplit;
    expect(p.kind).toBe('split');
    expect(p.direction).toBe('horizontal');
    expect(p.ratio).toBe(0.5);
    expect(isLeaf(p.first) && isLeaf(p.second)).toBe(true);
  });

  it('Grid → split{h,.5, split{v}, split{v}} with four empty leaves (F8)', () => {
    const p = buildPreset('Grid') as PaneSplit;
    expect(p.direction).toBe('horizontal');
    const l = p.first as PaneSplit;
    const r = p.second as PaneSplit;
    expect(l.direction).toBe('vertical');
    expect(r.direction).toBe('vertical');
    expect([l.first, l.second, r.first, r.second].every(isLeaf)).toBe(true);
    expect((l.first as PaneLeaf).tabs).toEqual([]);
  });

  it('exposes exactly the three built-ins in order', () => {
    expect(BUILTIN_PRESETS).toEqual(['Single', 'Side by side', 'Grid']);
  });
});

describe('capturePreset', () => {
  it('strips tabs but keeps the shape and ratio', () => {
    const live = makeSplit('vertical', makeLeaf([tab('t')], 'A'), makeLeaf([], 'B'), 0.4);
    const preset = capturePreset(live) as PaneSplit;
    expect(preset.ratio).toBe(0.4);
    expect(preset.direction).toBe('vertical');
    expect((preset.first as PaneLeaf).tabs).toEqual([]);
  });
});
