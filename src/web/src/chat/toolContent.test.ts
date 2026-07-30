import { describe, expect, it } from 'vitest';

import {
  kindIcon,
  parseToolContent,
  parseToolContents,
  parseToolLocations,
  statusGlyph,
  statusLabel,
  toolStatus,
} from './toolContent';

// SPEC-004 B19-B23. The wire shapes asserted here were read off
// agent-client-protocol-schema-1.5.0/src/v1/, so these tests double as the record
// of what the server actually forwards.

describe('parseToolContent', () => {
  it('narrows a diff with both sides', () => {
    expect(
      parseToolContent({ type: 'diff', path: 'src/a.rs', oldText: 'a\n', newText: 'b\n' }),
    ).toEqual({ type: 'diff', path: 'src/a.rs', oldText: 'a\n', newText: 'b\n' });
  });

  // `Diff.old_text` is `Option<String>` and skipped when None, so a new file
  // arrives with no key at all. Normalising to null keeps the renderer simple.
  it('normalises a missing oldText to null (new file)', () => {
    const parsed = parseToolContent({ type: 'diff', path: 'new.rs', newText: 'x\n' });
    expect(parsed).toMatchObject({ type: 'diff', oldText: null });
  });

  it('normalises an explicit null oldText to null', () => {
    const parsed = parseToolContent({ type: 'diff', path: 'n.rs', oldText: null, newText: 'x' });
    expect(parsed).toMatchObject({ oldText: null });
  });

  it('degrades a diff with no newText instead of throwing', () => {
    expect(parseToolContent({ type: 'diff', path: 'a.rs' })).toEqual({
      type: 'unknown',
      label: 'diff (incomplete)',
    });
  });

  it('narrows a terminal reference', () => {
    expect(parseToolContent({ type: 'terminal', terminalId: 'term-1' })).toEqual({
      type: 'terminal',
      terminalId: 'term-1',
    });
  });

  it('degrades a terminal with no id', () => {
    expect(parseToolContent({ type: 'terminal' })).toMatchObject({ type: 'unknown' });
  });

  it('narrows a wrapped text content block', () => {
    const parsed = parseToolContent({ type: 'content', content: { type: 'text', text: 'hi' } });
    expect(parsed).toEqual({ type: 'content', content: { type: 'text', text: 'hi' } });
  });

  it('keeps an image block with its mime type', () => {
    const parsed = parseToolContent({
      type: 'content',
      content: { type: 'image', data: 'AAAA', mimeType: 'image/png' },
    });
    expect(parsed).toMatchObject({ content: { mimeType: 'image/png' } });
  });

  it('degrades a content wrapper with no inner block', () => {
    expect(parseToolContent({ type: 'content' })).toMatchObject({ type: 'unknown' });
  });

  // ACP v2 adds an untagged `Other(String)` variant: an unrecognised tag is valid
  // protocol from a newer agent, so it must degrade, never throw.
  it('labels an unknown tag with the tag itself', () => {
    expect(parseToolContent({ type: 'hologram' })).toEqual({
      type: 'unknown',
      label: 'hologram',
    });
  });

  it('never throws on non-object input', () => {
    for (const raw of [null, undefined, 42, 'text', true, [], () => {}]) {
      expect(() => parseToolContent(raw)).not.toThrow();
    }
    expect(parseToolContent(null)).toMatchObject({ type: 'unknown' });
    expect(parseToolContent([])).toMatchObject({ type: 'unknown' });
  });

  it('labels an object with a non-string type', () => {
    expect(parseToolContent({ type: 7 })).toEqual({ type: 'unknown', label: 'unknown content' });
  });
});

describe('parseToolContents', () => {
  it('maps a whole array', () => {
    const out = parseToolContents([
      { type: 'content', content: { type: 'text', text: 'a' } },
      { type: 'diff', path: 'a', newText: 'b' },
    ]);
    expect(out.map((c) => c.type)).toEqual(['content', 'diff']);
  });

  it('returns an empty array for an absent or non-array field', () => {
    expect(parseToolContents(undefined)).toEqual([]);
    expect(parseToolContents(null)).toEqual([]);
    expect(parseToolContents({ 0: 'x' })).toEqual([]);
  });

  it('keeps one bad entry from discarding the good ones', () => {
    const out = parseToolContents([null, { type: 'terminal', terminalId: 't' }]);
    expect(out).toHaveLength(2);
    expect(out[1]).toMatchObject({ type: 'terminal' });
  });
});

describe('parseToolLocations', () => {
  it('keeps path and line', () => {
    expect(parseToolLocations([{ path: 'src/a.rs', line: 12 }])).toEqual([
      { path: 'src/a.rs', line: 12 },
    ]);
  });

  it('normalises a missing line to null', () => {
    expect(parseToolLocations([{ path: 'a.rs' }])).toEqual([{ path: 'a.rs', line: null }]);
  });

  it('drops entries with no usable path', () => {
    expect(parseToolLocations([{ line: 4 }, 'a.rs', null, { path: 7 }])).toEqual([]);
  });

  it('returns an empty array for a non-array', () => {
    expect(parseToolLocations(undefined)).toEqual([]);
  });
});

describe('toolStatus', () => {
  // The subtlest rule in the file: `Pending` is `#[default]` with
  // `skip_serializing_if = "is_default"`, so absent means pending, not unknown.
  it('reads an absent status as pending', () => {
    expect(toolStatus(undefined)).toEqual({ known: 'pending', raw: 'pending' });
  });

  it('passes the four known statuses through', () => {
    for (const s of ['pending', 'in_progress', 'completed', 'failed']) {
      expect(toolStatus(s).known).toBe(s);
    }
  });

  it('reports an unknown status as unknown but keeps the raw string', () => {
    expect(toolStatus('quantum')).toEqual({ known: null, raw: 'quantum' });
  });
});

describe('statusLabel', () => {
  it('labels the known statuses in Vietnamese', () => {
    expect(statusLabel(undefined)).toBe('đang chờ');
    expect(statusLabel('in_progress')).toBe('đang chạy');
    expect(statusLabel('completed')).toBe('xong');
    expect(statusLabel('failed')).toBe('lỗi');
  });

  it('shows an unknown status verbatim rather than hiding it', () => {
    expect(statusLabel('quantum')).toBe('quantum');
  });
});

describe('statusGlyph', () => {
  it('gives each known status a distinct glyph', () => {
    const glyphs = ['completed', 'failed', 'in_progress', 'pending'].map(statusGlyph);
    expect(new Set(glyphs).size).toBe(4);
  });

  it('marks an unknown status as a question mark, never as success', () => {
    expect(statusGlyph('quantum')).toBe('?');
    expect(statusGlyph('quantum')).not.toBe(statusGlyph('completed'));
  });
});

describe('kindIcon', () => {
  it('has an icon for every ToolKind in the schema', () => {
    const kinds = [
      'read',
      'edit',
      'delete',
      'move',
      'search',
      'execute',
      'think',
      'fetch',
      'switch_mode',
    ];
    for (const kind of kinds) expect(kindIcon(kind)).not.toBe('•');
  });

  it('falls back to a neutral icon for other and unknown kinds', () => {
    expect(kindIcon('other')).toBe('•');
    expect(kindIcon(undefined)).toBe('•');
    expect(kindIcon('teleport')).toBe('•');
  });
});
