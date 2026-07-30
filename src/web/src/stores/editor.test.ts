// Unit tests for the editor store (SPEC-002 test matrix, "FE unit").
//
// These cover the rules where a bug costs the user their edits rather than just
// looking wrong: auto-save on tab switch (`07:42`), and the 409 conflict path
// that must keep the tab dirty instead of dropping the only copy of the text
// ([INVENTED-9]).

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createPinia, setActivePinia } from 'pinia';

import { ApiError } from '../api/client';
import { useEditorStore } from './editor';

const { readFile, writeFile } = vi.hoisted(() => ({
  readFile: vi.fn(),
  writeFile: vi.fn(),
}));

// `conflictRev` is the real implementation on purpose: it is part of the
// behaviour under test (does a 409 body actually reach the store?).
vi.mock('../api/files', async () => {
  const actual = await vi.importActual<typeof import('../api/files')>('../api/files');
  return { ...actual, readFile, writeFile };
});

const PROJECT = 'p1';

function textRead(path: string, content: string, rev = 'r1') {
  return {
    kind: 'text' as const,
    path,
    size: content.length,
    mtimeMs: 1,
    rev,
    eol: 'lf' as const,
    content,
  };
}

describe('editor store', () => {
  beforeEach(() => {
    setActivePinia(createPinia());
    readFile.mockReset();
    writeFile.mockReset();
  });

  it('opens a file as a clean tab and keeps content out of the store', async () => {
    readFile.mockResolvedValue(textRead('a.ts', 'hello'));
    const store = useEditorStore();

    const result = await store.open(PROJECT, 'a.ts');

    expect(result).toEqual(textRead('a.ts', 'hello'));
    expect(store.tabs).toHaveLength(1);
    expect(store.activePath).toBe('a.ts');
    expect(store.tabs[0]).toMatchObject({ path: 'a.ts', name: 'a.ts', rev: 'r1', dirty: false });
    // The document must never be mirrored into reactive state (`07:40`).
    expect(JSON.stringify(store.tabs[0])).not.toContain('hello');
  });

  it('saves the outgoing tab when switching away from a dirty one', async () => {
    readFile
      .mockResolvedValueOnce(textRead('a.ts', 'A'))
      .mockResolvedValueOnce(textRead('b.ts', 'B', 'r2'));
    writeFile.mockResolvedValue({ rev: 'r1-new', size: 7, mtimeMs: 2 });

    const store = useEditorStore();
    store.setContentProvider((path) => (path === 'a.ts' ? 'A edited' : 'B'));

    await store.open(PROJECT, 'a.ts');
    store.markDirty('a.ts');
    await store.open(PROJECT, 'b.ts');

    expect(writeFile).toHaveBeenCalledWith(PROJECT, 'a.ts', 'A edited', 'r1');
    const a = store.tabs.find((t) => t.path === 'a.ts');
    expect(a).toMatchObject({ dirty: false, rev: 'r1-new' });
    expect(store.activePath).toBe('b.ts');
  });

  it('does not write when switching away from a clean tab', async () => {
    readFile
      .mockResolvedValueOnce(textRead('a.ts', 'A'))
      .mockResolvedValueOnce(textRead('b.ts', 'B'));
    const store = useEditorStore();
    store.setContentProvider(() => 'A');

    await store.open(PROJECT, 'a.ts');
    await store.open(PROJECT, 'b.ts');

    expect(writeFile).not.toHaveBeenCalled();
  });

  it('keeps the tab dirty and records the current rev on a 409', async () => {
    readFile.mockResolvedValue(textRead('a.ts', 'A'));
    writeFile.mockRejectedValue(
      new ApiError(409, 'file changed on disk', { error: 'conflict', currentRev: 'disk-9' }),
    );

    const store = useEditorStore();
    store.setContentProvider(() => 'A edited');
    await store.open(PROJECT, 'a.ts');
    store.markDirty('a.ts');

    const saved = await store.save(PROJECT, 'a.ts');

    expect(saved).toBe(false);
    // Dirty must survive: the in-editor text is the user's only copy.
    expect(store.tabs[0].dirty).toBe(true);
    expect(store.tabs[0].rev).toBe('r1');
    expect(store.conflict).toEqual({ path: 'a.ts', currentRev: 'disk-9' });
  });

  it('overwrite drops the rev precondition and clears the conflict', async () => {
    readFile.mockResolvedValue(textRead('a.ts', 'A'));
    writeFile
      .mockRejectedValueOnce(new ApiError(409, 'conflict', { currentRev: 'disk-9' }))
      .mockResolvedValueOnce({ rev: 'r-forced', size: 8, mtimeMs: 3 });

    const store = useEditorStore();
    store.setContentProvider(() => 'A edited');
    await store.open(PROJECT, 'a.ts');
    store.markDirty('a.ts');
    await store.save(PROJECT, 'a.ts');

    const ok = await store.overwrite(PROJECT, 'a.ts');

    expect(ok).toBe(true);
    // Force = no `rev` argument at all, which is what the server reads as
    // "overwrite regardless" ([INVENTED-9]).
    expect(writeFile).toHaveBeenLastCalledWith(PROJECT, 'a.ts', 'A edited', undefined);
    expect(store.conflict).toBeNull();
    expect(store.tabs[0]).toMatchObject({ dirty: false, rev: 'r-forced' });
  });

  it('never writes a binary or oversized tab', async () => {
    readFile.mockResolvedValue({
      kind: 'binary' as const,
      path: 'logo.png',
      size: 2048,
      mtimeMs: 1,
      rev: 'r1',
      mime: 'image/png',
    });
    const store = useEditorStore();
    store.setContentProvider(() => 'whatever');

    await store.open(PROJECT, 'logo.png');
    store.markDirty('logo.png');
    const saved = await store.save(PROJECT, 'logo.png');

    expect(saved).toBe(false);
    expect(writeFile).not.toHaveBeenCalled();
  });

  it('refuses to save when no document is available for the tab', async () => {
    readFile.mockResolvedValue(textRead('a.ts', 'A'));
    const store = useEditorStore();
    // No provider registered (pane not mounted): writing here would send empty
    // content over a real file.
    await store.open(PROJECT, 'a.ts');
    store.markDirty('a.ts');

    expect(await store.save(PROJECT, 'a.ts')).toBe(false);
    expect(writeFile).not.toHaveBeenCalled();
  });

  it('re-focuses an already open file instead of duplicating the tab', async () => {
    readFile
      .mockResolvedValueOnce(textRead('a.ts', 'A'))
      .mockResolvedValueOnce(textRead('b.ts', 'B'));
    const store = useEditorStore();

    await store.open(PROJECT, 'a.ts');
    await store.open(PROJECT, 'b.ts');
    readFile.mockClear();
    await store.open(PROJECT, 'a.ts');

    expect(store.tabs.map((t) => t.path)).toEqual(['a.ts', 'b.ts']);
    expect(store.activePath).toBe('a.ts');
    // Already-open means no second read: the parked CM6 state is the live one.
    expect(readFile).not.toHaveBeenCalled();
  });

  it('closing saves a dirty tab and activates a remaining one', async () => {
    readFile
      .mockResolvedValueOnce(textRead('a.ts', 'A', 'rev-a'))
      .mockResolvedValueOnce(textRead('b.ts', 'B', 'rev-b'));
    writeFile.mockResolvedValue({ rev: 'r-b', size: 1, mtimeMs: 2 });

    const store = useEditorStore();
    store.setContentProvider(() => 'B edited');
    await store.open(PROJECT, 'a.ts');
    await store.open(PROJECT, 'b.ts');
    store.markDirty('b.ts');

    await store.close(PROJECT, 'b.ts');

    // `rev-b`, not `rev-a`: the precondition must come from the tab being saved.
    expect(writeFile).toHaveBeenCalledWith(PROJECT, 'b.ts', 'B edited', 'rev-b');
    expect(store.tabs.map((t) => t.path)).toEqual(['a.ts']);
    expect(store.activePath).toBe('a.ts');
  });

  it('forget drops a deleted path and everything under it', async () => {
    readFile
      .mockResolvedValueOnce(textRead('src/a.ts', 'A'))
      .mockResolvedValueOnce(textRead('src/deep/b.ts', 'B'))
      .mockResolvedValueOnce(textRead('other.ts', 'C'));
    const store = useEditorStore();
    await store.open(PROJECT, 'src/a.ts');
    await store.open(PROJECT, 'src/deep/b.ts');
    await store.open(PROJECT, 'other.ts');

    store.forget('src');

    expect(store.tabs.map((t) => t.path)).toEqual(['other.ts']);
    expect(store.activePath).toBe('other.ts');
  });

  it('reset clears everything when the project changes', async () => {
    readFile.mockResolvedValue(textRead('a.ts', 'A'));
    const store = useEditorStore();
    await store.open(PROJECT, 'a.ts');

    store.reset();

    expect(store.tabs).toEqual([]);
    expect(store.activePath).toBeNull();
    expect(store.conflict).toBeNull();
  });
});
