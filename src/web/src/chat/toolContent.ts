// Narrowing `ToolCallPayload.content[]` (SPEC-004 §3.2).
//
// The server forwards whole ACP schema objects as opaque JSON (`acp/event.rs`
// `to_json`), so the browser receives `unknown[]` and has to narrow it here. The
// shapes below were read off `agent-client-protocol-schema-1.5.0/src/v1/`, not
// guessed: `ToolCallContent` is `#[serde(tag = "type", rename_all = "snake_case")]`
// and its payload structs are `rename_all = "camelCase"` + `skip_serializing_none`.
//
// Every enum in that schema is `#[non_exhaustive]`, and ACP v2 adds an untagged
// `Other(String)` variant to `status`/`kind`/content type. An unknown string is
// therefore VALID protocol, not corruption — so nothing here throws on one. That is
// why the narrowers return `null` and the components fall back to a neutral label.

/** `ToolCallContent::Diff` — `oldText` is absent/null for a new file. */
export interface DiffContent {
  type: 'diff';
  path: string;
  oldText?: string | null;
  newText: string;
}

/** `ToolCallContent::Terminal` — a reference to a terminal the agent owns. */
export interface TerminalContent {
  type: 'terminal';
  terminalId: string;
}

/** `ToolCallContent::Content` wrapping a `ContentBlock`. */
export interface BlockContent {
  type: 'content';
  content: ContentBlock;
}

/**
 * `ContentBlock`, also `tag = "type"` + snake_case.
 *
 * `resource` (embedded contents) and `audio` are recognised but not rendered this
 * phase — see [SPEC-004 INVENTED-6].
 */
export type ContentBlock =
  | { type: 'text'; text: string }
  | { type: 'image'; data: string; mimeType: string }
  | { type: 'audio'; data: string; mimeType: string }
  | { type: 'resource_link'; uri: string; name?: string; title?: string }
  | { type: 'resource'; resource: unknown }
  | { type: string };

/** Anything in `content[]`, including a shape this build does not know. */
export type ToolContent =
  | DiffContent
  | TerminalContent
  | BlockContent
  | { type: 'unknown'; label: string };

/** `locations[]` — used for jump-to-file. */
export interface ToolLocation {
  path: string;
  line?: number | null;
}

/** Narrow one raw entry. Never throws: unknown tags become an `unknown` item. */
export function parseToolContent(raw: unknown): ToolContent {
  if (!isRecord(raw)) return { type: 'unknown', label: 'malformed content' };

  switch (raw['type']) {
    case 'diff':
      // `newText` is required by the schema; without it there is nothing to show.
      if (typeof raw['path'] !== 'string' || typeof raw['newText'] !== 'string') {
        return { type: 'unknown', label: 'diff (incomplete)' };
      }
      return {
        type: 'diff',
        path: raw['path'],
        oldText: typeof raw['oldText'] === 'string' ? raw['oldText'] : null,
        newText: raw['newText'],
      };

    case 'terminal':
      if (typeof raw['terminalId'] !== 'string') {
        return { type: 'unknown', label: 'terminal (no id)' };
      }
      return { type: 'terminal', terminalId: raw['terminalId'] };

    case 'content':
      if (!isRecord(raw['content'])) return { type: 'unknown', label: 'content (empty)' };
      return { type: 'content', content: raw['content'] as ContentBlock };

    default:
      return {
        type: 'unknown',
        label: typeof raw['type'] === 'string' ? raw['type'] : 'unknown content',
      };
  }
}

/** Narrow a whole `content[]`. A non-array (or absent) field yields `[]`. */
export function parseToolContents(raw: unknown): ToolContent[] {
  return Array.isArray(raw) ? raw.map(parseToolContent) : [];
}

/** Narrow `locations[]`, dropping entries with no usable path. */
export function parseToolLocations(raw: unknown): ToolLocation[] {
  if (!Array.isArray(raw)) return [];
  const out: ToolLocation[] = [];
  for (const item of raw) {
    if (!isRecord(item) || typeof item['path'] !== 'string') continue;
    out.push({
      path: item['path'],
      line: typeof item['line'] === 'number' ? item['line'] : null,
    });
  }
  return out;
}

/** Known statuses. Absent means `pending` — it is the serde default and omitted. */
export type ToolStatus = 'pending' | 'in_progress' | 'completed' | 'failed';

const KNOWN_STATUSES: ReadonlySet<string> = new Set([
  'pending',
  'in_progress',
  'completed',
  'failed',
]);

/**
 * Normalise `status`.
 *
 * Returning `pending` for an absent value is protocol, not a guess:
 * `ToolCallStatus::Pending` is `#[default]` and `skip_serializing_if =
 * "is_default"`, so a pending call ships with no `status` key at all.
 */
export function toolStatus(status: string | undefined): { known: ToolStatus | null; raw: string } {
  if (status === undefined) return { known: 'pending', raw: 'pending' };
  return { known: KNOWN_STATUSES.has(status) ? (status as ToolStatus) : null, raw: status };
}

/** Vietnamese labels for the four known statuses; unknown strings pass through. */
export function statusLabel(status: string | undefined): string {
  const { known, raw } = toolStatus(status);
  switch (known) {
    case 'pending':
      return 'đang chờ';
    case 'in_progress':
      return 'đang chạy';
    case 'completed':
      return 'xong';
    case 'failed':
      return 'lỗi';
    default:
      return raw;
  }
}

/** A short glyph per status, for the collapsed group summary. */
export function statusGlyph(status: string | undefined): string {
  switch (toolStatus(status).known) {
    case 'completed':
      return '✓';
    case 'failed':
      return '✕';
    case 'in_progress':
      return '⋯';
    case 'pending':
      return '·';
    default:
      return '?';
  }
}

/** Icon per `ToolKind`. `other` and unknown kinds share the neutral one. */
export function kindIcon(kind: string | undefined): string {
  switch (kind) {
    case 'read':
      return '📄';
    case 'edit':
      return '✏️';
    case 'delete':
      return '🗑';
    case 'move':
      return '➜';
    case 'search':
      return '🔍';
    case 'execute':
      return '▶';
    case 'think':
      return '💭';
    case 'fetch':
      return '🌐';
    case 'switch_mode':
      return '⇄';
    default:
      return '•';
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
