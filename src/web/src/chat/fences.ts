// Detecting an unterminated code fence in streaming text (SPEC-004 §5.3).
//
// The problem this solves: text arrives token by token, so `renderMarkdown` is
// repeatedly called on markdown that is not yet syntactically complete. The
// moment "```rust\nfn ma" exists, markdown-it sees an unclosed fence and (per
// CommonMark) treats the rest of the document as code — then the closing fence
// arrives and the same text re-renders differently. Rendering the open tail as
// plain `<pre>` instead keeps the shape stable: closing the fence adds colour and
// nothing moves.
//
// CommonMark rules that matter here (and are easy to get wrong):
//  - A fence opens with at least 3 backticks or tildes, indented 0-3 spaces.
//  - The closing fence must use the SAME character and be at least as long as the
//    opener, which is how ```` ```` ```` can contain ``` verbatim.
//  - An opening fence may carry an info string; a closing fence may not.
//  - Tildes and backticks do not close each other.

/** Where the trailing unterminated fence starts, or `null` if all are closed. */
export interface OpenFence {
  /** Index in `text` of the line that opened the fence. */
  start: number;
  /** The info string as written (`rust`, `mermaid`, `ts title="a.ts"`). */
  info: string;
  /** Fence character and length, so a renderer can close it artificially. */
  marker: string;
}

const FENCE_LINE = /^ {0,3}(`{3,}|~{3,})(.*)$/;

/**
 * Find the unterminated fence at the end of `text`, if any.
 *
 * Scans linearly rather than tracking state across calls: chunks can be replayed
 * after a reconnect (`?after_seq=`), and a stateful counter would drift the first
 * time the same text is folded twice.
 */
export function openFence(text: string): OpenFence | null {
  let offset = 0;
  let open: OpenFence | null = null;

  for (const line of text.split('\n')) {
    const lineStart = offset;
    offset += line.length + 1;

    const match = FENCE_LINE.exec(line);
    if (!match) continue;

    const marker = match[1]!;
    const rest = match[2] ?? '';

    if (open === null) {
      // An info string containing a backtick is not a valid opener in CommonMark
      // (it would be ambiguous with inline code).
      if (marker.startsWith('`') && rest.includes('`')) continue;
      open = { start: lineStart, info: rest.trim(), marker };
      continue;
    }

    // Only the same character, at least as long, with nothing after it, closes.
    const sameChar = marker[0] === open.marker[0];
    const longEnough = marker.length >= open.marker.length;
    if (sameChar && longEnough && rest.trim() === '') open = null;
  }

  return open;
}

/** True when every fence in `text` is closed — safe to run mermaid/KaTeX. */
export function fencesBalanced(text: string): boolean {
  return openFence(text) === null;
}

/**
 * Split `text` into the part that is safe to render as markdown and the tail that
 * is still being streamed.
 *
 * The tail is returned as raw text so the caller can show it in a plain `<pre>`;
 * handing it to markdown-it is what produces the flicker.
 */
export function splitStreamingTail(text: string): { stable: string; tail: OpenFence & { code: string } | null } {
  const open = openFence(text);
  if (!open) return { stable: text, tail: null };

  const stable = text.slice(0, open.start);
  // Everything after the opening fence line is the code so far. The opener's own
  // line is dropped: its backticks are syntax, not content.
  const afterOpener = text.indexOf('\n', open.start);
  const code = afterOpener === -1 ? '' : text.slice(afterOpener + 1);

  return { stable, tail: { ...open, code } };
}
