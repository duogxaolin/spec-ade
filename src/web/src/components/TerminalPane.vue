<script setup lang="ts">
// One xterm.js terminal bound to one server-side PTY (SPEC-001 §5.7).
//
// The `Terminal` and `TerminalSocket` instances are held in plain module-local
// variables, NOT in `ref()`. Vue's reactive proxy would wrap xterm's internal
// buffers and break rendering — the same hazard the roadmap calls out for
// CodeMirror in Pha 2.

import { onBeforeUnmount, onMounted, ref, useTemplateRef, watch } from 'vue';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';

import { TerminalSocket, type ConnectionState } from '../api/terminalSocket';

const props = defineProps<{
  terminalId: string;
  /** Byte offset to resume from; omit to replay all history. */
  afterSeq?: number;
}>();

const emit = defineEmits<{
  cwd: [path: string];
  exit: [code: number | null];
  error: [message: string];
}>();

const host = useTemplateRef<HTMLDivElement>('host');
const connection = ref<ConnectionState>('connecting');
const exited = ref<{ code: number | null; signal: string | null } | null>(null);

let term: Terminal | null = null;
let fit: FitAddon | null = null;
let socket: TerminalSocket | null = null;
let observer: ResizeObserver | null = null;

function mountTerminal(): void {
  if (!host.value) return;

  term = new Terminal({
    // Terminal apps assume a fixed-width grid; a proportional font would break
    // every box-drawing character and column alignment.
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
    fontSize: 13,
    // Server-side scrollback is the source of truth for replay; this is just
    // how much the local view keeps for scrolling.
    scrollback: 5000,
    cursorBlink: true,
    // Bracketed paste and other escapes are forwarded verbatim by the server
    // (deep-dive 02 §5.2), so xterm's own handling works as designed.
    allowProposedApi: false,
  });
  fit = new FitAddon();
  term.loadAddon(fit);
  term.open(host.value);

  socket = new TerminalSocket(
    props.terminalId,
    {
      onOutput: (data) => term?.write(data),
      onReady: () => {
        // Size the PTY to the pane now that the socket is live; the shell was
        // spawned at whatever default the caller asked for.
        syncSize();
      },
      onCwd: (path) => emit('cwd', path),
      onExit: (msg) => {
        exited.value = msg;
        emit('exit', msg.code);
      },
      onTruncated: () => {
        // Be explicit: the user is looking at a stream with a hole in it.
        term?.write('\r\n\x1b[2m[scrollback truncated by server]\x1b[0m\r\n');
      },
      onServerError: (message) => emit('error', message),
      onStateChange: (state) => {
        connection.value = state;
      },
    },
    { afterSeq: props.afterSeq },
  );
  socket.connect();

  // Keystrokes and paste. `onData` yields UTF-8 text; `onBinary` yields the
  // rarer raw-byte path (e.g. some mouse/encoding modes).
  term.onData((data) => socket?.sendInput(data));
  term.onBinary((data) => {
    const bytes = new Uint8Array(data.length);
    for (let i = 0; i < data.length; i += 1) bytes[i] = data.charCodeAt(i) & 0xff;
    socket?.sendBytes(bytes);
  });

  // Fit on container resize rather than on window resize: a pane can change
  // size from a split drag with the window untouched (Pha 8 splits panes).
  observer = new ResizeObserver(() => syncSize());
  observer.observe(host.value);
  syncSize();
}

/** Re-fit the grid and tell the server the new size. */
function syncSize(): void {
  if (!fit || !term || !host.value) return;
  // `fit()` divides by cell size; a hidden pane has zero height and would
  // compute a nonsense grid.
  if (host.value.clientWidth === 0 || host.value.clientHeight === 0) return;
  fit.fit();
  socket?.resize(term.rows, term.cols);
}

function teardown(): void {
  observer?.disconnect();
  observer = null;
  // Close the socket first so no write lands on a disposed terminal.
  socket?.dispose();
  socket = null;
  term?.dispose();
  term = null;
  fit = null;
}

onMounted(mountTerminal);
onBeforeUnmount(teardown);

// Switching which PTY this pane shows means a full rebuild: the scrollback,
// cursor state and socket all belong to the old terminal.
watch(
  () => props.terminalId,
  () => {
    teardown();
    exited.value = null;
    mountTerminal();
  },
);

/** Focus the terminal — used when its tab becomes active. */
defineExpose({
  focus: () => term?.focus(),
  refit: syncSize,
});
</script>

<template>
  <div class="terminal-pane">
    <div ref="host" class="terminal-pane__host" />

    <div v-if="connection === 'reconnecting'" class="terminal-pane__banner">
      Reconnecting…
    </div>
    <div v-else-if="exited" class="terminal-pane__banner terminal-pane__banner--exited">
      Shell exited{{ exited.signal ? ` (signal ${exited.signal})` : ` (code ${exited.code})` }}
    </div>
  </div>
</template>

<style scoped>
.terminal-pane {
  position: relative;
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
  background: #1e1e1e;
}
.terminal-pane__host {
  flex: 1;
  min-height: 0;
  padding: 4px;
}
.terminal-pane__banner {
  position: absolute;
  right: 8px;
  bottom: 8px;
  padding: 2px 8px;
  border-radius: 3px;
  font: 600 12px/1.6 system-ui, sans-serif;
  background: #4a3c00;
  color: #ffd24a;
}
.terminal-pane__banner--exited {
  background: #3a1f1f;
  color: #ff9b9b;
}
</style>
