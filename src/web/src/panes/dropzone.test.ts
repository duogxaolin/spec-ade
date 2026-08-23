import { describe, it, expect } from 'vitest';
import { resolveZone, resolveDrop, TABSTRIP_HEIGHT, type Rect } from './dropzone';

const rect: Rect = { x: 0, y: 0, width: 400, height: 300 };
const at = (x: number, y: number) => ({ x, y });

describe('resolveZone (F10-F12)', () => {
  it('center of the rect → center (F10)', () => {
    expect(resolveZone(rect, at(200, 150))).toBe('center');
  });

  it('20% edge bands → directional split (F11)', () => {
    expect(resolveZone(rect, at(20, 150))).toBe('left'); // fx = 0.05
    expect(resolveZone(rect, at(380, 150))).toBe('right'); // fx = 0.95
    expect(resolveZone(rect, at(200, 45))).toBe('up'); // fy = 0.15, below the strip
    expect(resolveZone(rect, at(200, 285))).toBe('down'); // fy = 0.95
  });

  it('top 32px → tabstrip (F12)', () => {
    expect(resolveZone(rect, at(200, 10))).toBe('tabstrip');
    expect(resolveZone(rect, at(200, TABSTRIP_HEIGHT))).toBe('tabstrip');
  });

  it('nearest edge wins near a corner (deterministic)', () => {
    expect(resolveZone(rect, at(5, 150))).toBe('left');
  });
});

describe('resolveDrop no-op guard (F13)', () => {
  it('blocks dropping a tab back on its own single-tab leaf', () => {
    expect(resolveDrop(rect, at(200, 150), { sameLeaf: true, targetTabCount: 1 }).noop).toBe(true);
  });
  it('allows a real cross-leaf drop', () => {
    const r = resolveDrop(rect, at(200, 150), { sameLeaf: false, targetTabCount: 1 });
    expect(r.noop).toBe(false);
    expect(r.zone).toBe('center');
  });
  it('allows reordering within a multi-tab leaf', () => {
    const r = resolveDrop(rect, at(200, 10), { sameLeaf: true, targetTabCount: 3 });
    expect(r.noop).toBe(false);
    expect(r.zone).toBe('tabstrip');
  });
});
