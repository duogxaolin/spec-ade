// Debounced markdown rendering for streaming text (SPEC-004 §5.3).
//
// A turn delivers hundreds of `message_chunk` frames. Rendering on each one means
// re-parsing the whole reply per token: at a few hundred characters that is fine,
// at ten thousand it drops frames. Coalescing into one render per ~50 ms bounds the
// work to 20 parses a second regardless of how fast the agent streams.
//
// The window is 50 ms, mid-range of the 30-60 ms the roadmap specifies. Below ~30
// the saving disappears; above ~60 the text visibly lags the agent.
//
// Two exceptions to the debounce, both deliberate:
//  - the FIRST render is immediate, so a reply appears the instant it starts;
//  - `flush()` renders synchronously when the turn ends, so the final words are
//    never left waiting on a timer.

/** Milliseconds to coalesce chunks into one render. */
export const RENDER_DEBOUNCE_MS = 50;

export interface DebouncedRenderer {
  /** Feed the latest full text. Renders now or on the next tick. */
  update(source: string): void;
  /** Render immediately, cancelling any pending timer. */
  flush(): void;
  /** Drop any pending render. Call on unmount. */
  dispose(): void;
}

/**
 * Wrap `render` so rapid `update` calls collapse into one call per window.
 *
 * Takes the whole text each time rather than a delta: the transcript fold already
 * accumulates into one string per block, and diffing deltas here would duplicate
 * that with a second source of truth.
 */
export function createDebouncedRenderer(
  render: (source: string) => void,
  waitMs: number = RENDER_DEBOUNCE_MS,
): DebouncedRenderer {
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: string | null = null;
  let rendered = false;

  function run(): void {
    timer = null;
    if (pending === null) return;
    const source = pending;
    pending = null;
    rendered = true;
    render(source);
  }

  return {
    update(source: string): void {
      pending = source;
      // First paint is not debounced: waiting 50 ms to show that the agent has
      // started replying reads as lag, not as smoothing.
      if (!rendered) {
        if (timer !== null) clearTimeout(timer);
        run();
        return;
      }
      timer ??= setTimeout(run, waitMs);
    },

    flush(): void {
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      run();
    },

    dispose(): void {
      if (timer !== null) clearTimeout(timer);
      timer = null;
      pending = null;
    },
  };
}
