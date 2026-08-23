import { describe, it, expect } from 'vitest';
import { touch, remove, pickNext, mruSelector } from './mru';
import type { TabDescriptor } from './tree';

function tab(id: string): TabDescriptor {
  return { id, kind: 'file', title: id, params: {} };
}

describe('touch', () => {
  it('moves an id to the front and dedups', () => {
    expect(touch(['b', 'a'], 'a')).toEqual(['a', 'b']);
    expect(touch(['a', 'b'], 'c')).toEqual(['c', 'a', 'b']);
  });
});

describe('remove', () => {
  it('drops an id; no-op if absent', () => {
    expect(remove(['a', 'b'], 'a')).toEqual(['b']);
    expect(remove(['a', 'b'], 'z')).toEqual(['a', 'b']);
  });
});

describe('pickNext (F5)', () => {
  const surviving = [tab('t1'), tab('t3'), tab('t4')]; // t2 already filtered out

  it('returns the top-of-stack survivor', () => {
    expect(pickNext(['t4', 't3'], surviving)).toBe('t4');
  });
  it('skips a stale id for the just-closed tab', () => {
    expect(pickNext(['t2', 't3'], surviving)).toBe('t3'); // t2 gone → next survivor
  });
  it('falls back to the last surviving tab when the stack is empty', () => {
    expect(pickNext([], surviving)).toBe('t4');
  });
  it('returns null when nothing survives', () => {
    expect(pickNext(['x'], [])).toBeNull();
  });
});

describe('mruSelector', () => {
  it('produces a closeTab-compatible selector', () => {
    expect(mruSelector(['t3'])([tab('t1'), tab('t3')])).toBe('t3');
  });
});
