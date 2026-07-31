import { describe, it, expect } from 'vitest';
import {
  sparklinePath,
  sparklineArea,
  pushPoint,
  formatBytes,
  formatUptime,
  formatPercent,
  HISTORY_LIMIT,
} from './sparkline';

/** The failure this guards against: one NaN and the whole `<path>` vanishes. */
function hasNaN(path: string): boolean {
  return /NaN|Infinity|undefined/.test(path);
}

describe('sparklinePath', () => {
  it('is empty for no points, rather than a malformed d (D39)', () => {
    expect(sparklinePath([])).toBe('');
    expect(hasNaN(sparklinePath([]))).toBe(false);
  });

  it('draws a flat full-width segment for a single point (D39)', () => {
    const path = sparklinePath([50], { width: 100, height: 20, max: 100 });
    expect(hasNaN(path)).toBe(false);
    // Not a zero-length path: it must span the width or nothing is visible.
    expect(path).toBe('M 0 10 L 100 10');
  });

  it('produces no NaN when every point is equal (D39)', () => {
    // max === min → the scale divides by zero unless it is special-cased.
    for (const flat of [[0, 0, 0], [7, 7, 7, 7], [100, 100]]) {
      const path = sparklinePath(flat);
      expect(hasNaN(path), `flat series ${flat[0]}`).toBe(false);
      expect(path.length).toBeGreaterThan(0);
    }
  });

  it('pins an all-zero series to the baseline', () => {
    // An idle machine reports 0.0% forever; the line belongs at the bottom.
    expect(sparklinePath([0, 0, 0], { width: 100, height: 20 })).toBe(
      'M 0 20 L 50 20 L 100 20',
    );
  });

  it('spreads points across the width and inverts the y axis', () => {
    // SVG y grows downward, so the largest value must have the smallest y.
    const path = sparklinePath([0, 50, 100], { width: 100, height: 20, max: 100 });
    expect(path).toBe('M 0 20 L 50 10 L 100 0');
  });

  it('does not rebase on the series minimum by default', () => {
    // 41 → 42 on a fixed 0-baseline is a hair; on a rebased one it is full-scale.
    const path = sparklinePath([41, 42], { width: 100, height: 20, max: 100 });
    expect(path).toBe('M 0 11.8 L 100 11.6');
  });

  it('clamps a value above max instead of drawing outside the box', () => {
    const path = sparklinePath([0, 780], { width: 10, height: 20, max: 100 });
    expect(path).toBe('M 0 20 L 10 0');
  });

  it('never emits NaN for any history length up to the limit', () => {
    for (let n = 0; n <= HISTORY_LIMIT; n += 1) {
      const values = Array.from({ length: n }, (_, i) => i % 3);
      expect(hasNaN(sparklinePath(values)), `length ${n}`).toBe(false);
    }
  });
});

describe('sparklineArea', () => {
  it('is empty when the line is empty (D39)', () => {
    expect(sparklineArea([])).toBe('');
  });

  it('closes the path along the baseline', () => {
    const area = sparklineArea([0, 100], { width: 100, height: 20, max: 100 });
    expect(area).toBe('M 0 20 L 100 0 L 100 20 L 0 20 Z');
    expect(hasNaN(area)).toBe(false);
  });
});

describe('pushPoint', () => {
  it('keeps at most the limit, dropping the oldest (D44 support)', () => {
    let history: number[] = [];
    for (let i = 0; i < HISTORY_LIMIT + 10; i += 1) {
      history = pushPoint(history, i);
    }
    expect(history).toHaveLength(HISTORY_LIMIT);
    expect(history[0]).toBe(10);
    expect(history[history.length - 1]).toBe(HISTORY_LIMIT + 9);
  });

  it('returns a new array so reactivity sees the change', () => {
    const before: number[] = [];
    expect(pushPoint(before, 1)).not.toBe(before);
    expect(before).toHaveLength(0);
  });
});

describe('formatBytes', () => {
  it('is right at the unit boundaries (D40)', () => {
    expect(formatBytes(1023)).toBe('1023 B');
    expect(formatBytes(1024)).toBe('1.0 KB');
    expect(formatBytes(1024 * 1024)).toBe('1.0 MB');
    expect(formatBytes(1024 * 1024 - 1)).toBe('1024.0 KB');
  });

  it('uses binary units, matching what sysinfo reports', () => {
    // 16 GiB. Dividing by 1000 would print "17.2 GB" for a 16 GB machine.
    expect(formatBytes(17179869184)).toBe('16.0 GB');
  });

  it('shows whole bytes without a decimal', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(512)).toBe('512 B');
  });

  it('stops at the largest unit rather than running off the table', () => {
    expect(formatBytes(1024 ** 6)).toBe('1024.0 PB');
  });

  it('handles negatives and non-finite input', () => {
    expect(formatBytes(-2048)).toBe('-2.0 KB');
    expect(formatBytes(Number.NaN)).toBe('—');
  });
});

describe('formatUptime', () => {
  it('shows the two largest units', () => {
    expect(formatUptime(0)).toBe('0s');
    expect(formatUptime(45)).toBe('45s');
    expect(formatUptime(65)).toBe('1m 5s');
    expect(formatUptime(3600)).toBe('1h 0m');
    expect(formatUptime(3 * 86400 + 4 * 3600)).toBe('3d 4h');
  });

  it('rejects nonsense rather than rendering it', () => {
    expect(formatUptime(-1)).toBe('—');
    expect(formatUptime(Number.NaN)).toBe('—');
  });
});

describe('formatPercent', () => {
  it('formats and tolerates over-100 process usage', () => {
    expect(formatPercent(41.23)).toBe('41.2%');
    expect(formatPercent(780)).toBe('780.0%');
    expect(formatPercent(Number.NaN)).toBe('—');
  });
});
