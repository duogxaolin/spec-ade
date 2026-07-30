import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { RENDER_DEBOUNCE_MS, createDebouncedRenderer } from './render';

// SPEC-004 B10-B11: the debounce contract. Fake timers, so the assertions are on
// exact call counts rather than on wall-clock luck.

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe('createDebouncedRenderer', () => {
  it('renders the first update immediately', () => {
    const render = vi.fn();
    createDebouncedRenderer(render).update('a');
    expect(render).toHaveBeenCalledExactlyOnceWith('a');
  });

  it('coalesces a burst after the first render into one call', () => {
    const render = vi.fn();
    const r = createDebouncedRenderer(render);

    r.update('a'); // immediate
    for (const text of ['ab', 'abc', 'abcd', 'abcde']) r.update(text);
    expect(render).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(RENDER_DEBOUNCE_MS);
    expect(render).toHaveBeenCalledTimes(2);
    // The newest text wins: intermediate states are never rendered.
    expect(render).toHaveBeenLastCalledWith('abcde');
  });

  it('does not render before the window elapses', () => {
    const render = vi.fn();
    const r = createDebouncedRenderer(render, 50);
    r.update('a');
    r.update('ab');
    vi.advanceTimersByTime(49);
    expect(render).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(1);
    expect(render).toHaveBeenCalledTimes(2);
  });

  // Trailing-edge, not leading: a second burst inside the same window must not
  // reset the timer, or fast streaming would starve the render forever.
  it('does not let a continuous stream postpone the render', () => {
    const render = vi.fn();
    const r = createDebouncedRenderer(render, 50);
    r.update('a');
    for (let i = 0; i < 10; i++) {
      r.update(`a${i}`);
      vi.advanceTimersByTime(10);
    }
    // 100 ms of continuous updates ⇒ two windows fired, not zero.
    expect(render.mock.calls.length).toBeGreaterThanOrEqual(3);
  });

  it('flushes pending text synchronously', () => {
    const render = vi.fn();
    const r = createDebouncedRenderer(render);
    r.update('a');
    r.update('final');
    r.flush();
    expect(render).toHaveBeenCalledTimes(2);
    expect(render).toHaveBeenLastCalledWith('final');
    // Nothing left on a timer after a flush.
    vi.advanceTimersByTime(1000);
    expect(render).toHaveBeenCalledTimes(2);
  });

  it('flush is a no-op when nothing is pending', () => {
    const render = vi.fn();
    const r = createDebouncedRenderer(render);
    r.update('a');
    r.flush();
    r.flush();
    expect(render).toHaveBeenCalledTimes(1);
  });

  it('drops the pending render on dispose', () => {
    const render = vi.fn();
    const r = createDebouncedRenderer(render);
    r.update('a');
    r.update('b');
    r.dispose();
    vi.advanceTimersByTime(1000);
    expect(render).toHaveBeenCalledTimes(1);
    expect(render).not.toHaveBeenCalledWith('b');
  });

  it('renders an empty first update rather than skipping it', () => {
    // An empty message block is a real state (chunk arrived with no text yet) and
    // the caller decides what to show; the renderer must not swallow it.
    const render = vi.fn();
    createDebouncedRenderer(render).update('');
    expect(render).toHaveBeenCalledExactlyOnceWith('');
  });

  it('honours a custom window', () => {
    const render = vi.fn();
    const r = createDebouncedRenderer(render, 200);
    r.update('a');
    r.update('b');
    vi.advanceTimersByTime(50);
    expect(render).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(150);
    expect(render).toHaveBeenCalledTimes(2);
  });

  it('keeps the documented default window', () => {
    expect(RENDER_DEBOUNCE_MS).toBe(50);
  });
});
