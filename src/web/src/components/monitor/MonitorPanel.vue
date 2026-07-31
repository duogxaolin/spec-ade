<script setup lang="ts">
// The monitor pane: CPU/RAM/GPU cards with sparklines, plus the process list.
//
// The stream starts on mount and stops on unmount, because the server keeps
// sampling while at least one subscriber is attached (`IDLE_GRACE`) — a pane
// nobody is looking at should not hold the sampler open.
//
// `watchMode` is shown rather than hidden: once the fallback has kicked in (D45)
// the panel is 3 s-polled instead of streamed, and a user reading a number that is
// only *nearly* live should be able to tell.

import { computed, onBeforeUnmount, onMounted } from 'vue';

import type { KillSignalName, SortBy } from '../../api/system';
import { formatBytes, formatPercent, formatUptime } from '../../monitor/sparkline';
import { useMonitorStore } from '../../stores/monitor';
import ProcessList from './ProcessList.vue';
import Sparkline from './Sparkline.vue';

const store = useMonitorStore();

const host = computed(() => store.metrics?.host ?? null);
const cpu = computed(() => store.metrics?.cpu ?? null);
const memory = computed(() => store.metrics?.memory ?? null);
const gpu = computed(() => store.metrics?.gpu ?? null);

const modeLabel = computed(() => {
  switch (store.watchMode) {
    case 'live':
      return 'trực tiếp';
    case 'polling':
      return 'poll 3s';
    default:
      return 'dừng';
  }
});

function onKill(pid: number, signal: KillSignalName): void {
  void store.kill(pid, signal);
}

onMounted(() => store.startWatch());
onBeforeUnmount(() => store.stopWatch());
</script>

<template>
  <div class="mon">
    <header class="mon__head">
      <span v-if="host" class="mon__host">{{ host.name }} · {{ host.os }}</span>
      <span class="mon__spacer" />
      <span class="mon__mode" :class="`mon__mode--${store.watchMode}`">{{ modeLabel }}</span>
    </header>

    <p v-if="store.error" class="mon__error">{{ store.error }}</p>

    <div class="mon__cards">
      <section class="mon__card">
        <div class="mon__cardhead">
          <span class="mon__label">CPU</span>
          <span class="mon__value">{{ cpu ? formatPercent(cpu.usage) : '—' }}</span>
        </div>
        <Sparkline :values="store.cpuHistory" :max="100" color="#7a9ec4" label="CPU" />
        <div class="mon__sub">
          <span v-if="cpu">{{ cpu.coreCount }} nhân</span>
          <span v-if="host">tải {{ host.loadAvg.map((l) => l.toFixed(2)).join(' ') }}</span>
        </div>
      </section>

      <section class="mon__card">
        <div class="mon__cardhead">
          <span class="mon__label">RAM</span>
          <span class="mon__value">{{ formatPercent(store.memoryPercent) }}</span>
        </div>
        <Sparkline :values="store.memoryHistory" :max="100" color="#6fcf74" label="RAM" />
        <div class="mon__sub">
          <span v-if="memory">
            {{ formatBytes(memory.used) }} / {{ formatBytes(memory.total) }}
          </span>
          <span v-if="memory && memory.swapTotal > 0">
            swap {{ formatBytes(memory.swapUsed) }}
          </span>
        </div>
      </section>

      <!-- Absent on hosts with no readable GPU; a zeroed card would claim a
           measurement that was never taken. -->
      <section v-if="gpu" class="mon__card">
        <div class="mon__cardhead">
          <span class="mon__label">GPU</span>
          <span class="mon__value">{{ formatPercent(gpu.usage) }}</span>
        </div>
        <Sparkline :values="store.gpuHistory" :max="100" color="#d3a83c" label="GPU" />
        <div class="mon__sub">
          <span>{{ gpu.name }}</span>
          <span v-if="gpu.memoryTotal > 0">
            {{ formatBytes(gpu.memoryUsed) }} / {{ formatBytes(gpu.memoryTotal) }}
          </span>
          <span v-if="gpu.temperatureC !== null">{{ gpu.temperatureC }}°C</span>
        </div>
      </section>

      <section class="mon__card">
        <div class="mon__cardhead">
          <span class="mon__label">Uptime</span>
          <span class="mon__value">{{ host ? formatUptime(host.uptimeSec) : '—' }}</span>
        </div>
        <div class="mon__sub">
          <span v-if="store.metrics">{{ store.metrics.processCount }} tiến trình</span>
        </div>
      </section>
    </div>

    <ProcessList
      class="mon__procs"
      :processes="store.processes"
      :sort="store.sort"
      :filter="store.filter"
      :total="store.metrics?.processCount"
      :truncated="store.metrics?.truncated"
      :busy="store.busy"
      @kill="onKill"
      @update:sort="(s: SortBy) => store.setView({ sort: s })"
      @update:filter="(f: string) => (store.filter = f)"
    />
  </div>
</template>

<style scoped>
.mon {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-height: 0;
  gap: 6px;
  padding: 6px;
  overflow: auto;
}
.mon__head {
  display: flex;
  align-items: center;
  gap: 6px;
  color: #7a7a7a;
  font-size: 11px;
}
.mon__spacer {
  flex: 1;
}
.mon__mode--live {
  color: #6fcf74;
}
.mon__mode--polling {
  color: #d3a83c;
}
.mon__error {
  margin: 0;
  color: #e06c6c;
  font-size: 11px;
}
.mon__cards {
  display: grid;
  gap: 6px;
  grid-template-columns: repeat(auto-fit, minmax(140px, 1fr));
}
.mon__card {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding: 6px;
  border: 1px solid #2c2c2c;
  border-radius: 4px;
  background: #1c1c1c;
}
.mon__cardhead {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
}
.mon__label {
  color: #7a7a7a;
  font-size: 11px;
  text-transform: uppercase;
}
.mon__value {
  color: #e4e4e4;
  font-size: 14px;
  font-variant-numeric: tabular-nums;
}
.mon__sub {
  display: flex;
  gap: 8px;
  color: #7a7a7a;
  font-size: 11px;
}
.mon__procs {
  flex: 1;
  min-height: 0;
}
</style>
