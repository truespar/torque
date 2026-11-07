<template>
  <div class="tq-panel">
    <h2>Dashboard</h2>

    <div class="dashboard__status">
      <div class="tq-card">
        <div class="dashboard__stat-label">Status</div>
        <span class="tq-badge" :class="connection.isConnected ? 'tq-badge--success' : 'tq-badge--error'">
          {{ connection.isConnected ? 'Connected' : 'Disconnected' }}
        </span>
      </div>
      <div class="tq-card">
        <div class="dashboard__stat-label">Collections</div>
        <div class="dashboard__stat-value">{{ connection.collectionList.length }}</div>
      </div>
      <div class="tq-card">
        <div class="dashboard__stat-label">Total Documents</div>
        <div class="dashboard__stat-value">{{ totalDocs.toLocaleString() }}</div>
      </div>
      <div class="tq-card">
        <div class="dashboard__stat-label">Uptime</div>
        <div class="dashboard__stat-value">{{ formatUptime(connection.uptime) }}</div>
      </div>
    </div>

    <h3>Collections</h3>
    <div class="dashboard__collections">
      <div v-for="col in connection.collectionList" :key="col.name" class="tq-card dashboard__col-card" @click="selectCollection(col.name)">
        <div class="dashboard__col-name">
          {{ col.name }}
          <span v-if="col.index_mode === 'realtime'" class="dashboard__rt-badge">RT</span>
        </div>
        <div class="dashboard__col-docs">{{ (col.num_documents ?? 0).toLocaleString() }} documents</div>
        <div class="dashboard__col-fields">{{ col.fields?.length ?? 0 }} fields</div>
      </div>
      <div v-if="!connection.collectionList.length" class="tq-empty">
        <span>No collections yet</span>
      </div>
    </div>

    <h3 v-if="connection.memory">Memory Usage</h3>
    <div v-if="connection.memory" class="tq-card">
      <div class="dashboard__memory-row">
        <span>Process Resident</span>
        <span class="mono">{{ formatBytes(connection.memory.resident_bytes || 0) }}</span>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed } from 'vue'
import { useConnectionStore } from '@/stores/connection'
import { useSettingsStore } from '@/stores/settings'

const connection = useConnectionStore()
const settings = useSettingsStore()

const totalDocs = computed(() =>
  connection.collectionList.reduce((sum: number, c: any) => sum + (c.num_documents ?? 0), 0)
)

function selectCollection(name: string) {
  connection.setCurrentCollection(name)
  settings.activePanel = null
}

function formatUptime(secs: number): string {
  if (secs < 60) return `${secs}s`
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`
  const h = Math.floor(secs / 3600)
  const m = Math.floor((secs % 3600) / 60)
  return `${h}h ${m}m`
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return bytes + ' B'
  if (bytes < 1048576) return (bytes / 1024).toFixed(1) + ' KB'
  if (bytes < 1073741824) return (bytes / 1048576).toFixed(1) + ' MB'
  return (bytes / 1073741824).toFixed(2) + ' GB'
}
</script>

<style scoped>
.dashboard__status {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(180px, 1fr));
  gap: 12px;
  margin-bottom: 24px;
}

.dashboard__stat-label {
  font-size: var(--tq-font-size-xs);
  color: var(--tq-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.5px;
  margin-bottom: 8px;
}

.dashboard__stat-value {
  font-size: var(--tq-font-size-2xl);
  font-weight: 600;
}

.dashboard__collections {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
  gap: 12px;
  margin-bottom: 24px;
}

.dashboard__col-card {
  cursor: pointer;
  transition: border-color var(--tq-transition);
}

.dashboard__col-card:hover {
  border-color: var(--tq-accent);
}

.dashboard__col-name {
  display: flex;
  align-items: center;
  gap: 6px;
  font-weight: 600;
  margin-bottom: 6px;
}

.dashboard__rt-badge {
  font-size: 10px;
  font-weight: 600;
  padding: 1px 5px;
  border-radius: 3px;
  background: var(--tq-accent);
  color: var(--tq-text-inverse, #fff);
  letter-spacing: 0.04em;
}

.dashboard__col-docs {
  font-size: var(--tq-font-size-sm);
  color: var(--tq-text-secondary);
}

.dashboard__col-fields {
  font-size: var(--tq-font-size-xs);
  color: var(--tq-text-muted);
}

.dashboard__memory-row {
  display: flex;
  justify-content: space-between;
  padding: 6px 0;
  font-size: var(--tq-font-size-sm);
}
</style>
