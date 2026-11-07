<template>
  <div class="facet-panel">
    <h3>Facets</h3>
    <div v-for="facet in facets" :key="facet.field_name" class="facet-panel__group">
      <div class="facet-panel__field">{{ facet.field_name }}</div>
      <div
        v-for="val in facet.counts"
        :key="val.value"
        class="facet-panel__value"
        @click="addFilter(facet.field_name, val.value)"
        :title="`Click to filter by ${facet.field_name}:=${val.value}`"
      >
        <span class="facet-panel__label">{{ val.value }}</span>
        <div class="facet-panel__bar-wrapper">
          <div class="facet-panel__bar" :style="{ width: barWidth(val.count, facet.counts) }" />
        </div>
        <span class="facet-panel__count">{{ val.count }}</span>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { useQueryStore } from '@/stores/query'

defineProps<{ facets: any[] }>()
const queryStore = useQueryStore()

function barWidth(count: number, counts: any[]): string {
  const max = Math.max(...counts.map((c: any) => c.count))
  return max > 0 ? `${(count / max) * 100}%` : '0%'
}

function addFilter(field: string, value: string) {
  try {
    const parsed = JSON.parse(queryStore.currentQuery)
    const filterExpr = `${field}:=${value}`
    if (parsed.filter_by) {
      parsed.filter_by += ` && ${filterExpr}`
    } else {
      parsed.filter_by = filterExpr
    }
    queryStore.currentQuery = JSON.stringify(parsed, null, 2)
  } catch {}
}
</script>

<style scoped>
.facet-panel {
  width: 240px;
  border-left: 1px solid var(--tq-border);
  padding: 12px;
  overflow-y: auto;
  flex-shrink: 0;
}

.facet-panel h3 {
  font-size: var(--tq-font-size-sm);
  font-weight: 600;
  color: var(--tq-text-secondary);
  margin-bottom: 12px;
}

.facet-panel__group {
  margin-bottom: 16px;
}

.facet-panel__field {
  font-size: var(--tq-font-size-xs);
  font-weight: 600;
  color: var(--tq-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.5px;
  margin-bottom: 6px;
}

.facet-panel__value {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 3px 0;
  cursor: pointer;
  font-size: var(--tq-font-size-xs);
  transition: color var(--tq-transition);
}

.facet-panel__value:hover {
  color: var(--tq-accent);
}

.facet-panel__label {
  min-width: 60px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.facet-panel__bar-wrapper {
  flex: 1;
  height: 6px;
  background: var(--tq-bg-hover);
  border-radius: 3px;
  overflow: hidden;
}

.facet-panel__bar {
  height: 100%;
  background: var(--tq-accent);
  border-radius: 3px;
  transition: width 0.3s ease;
}

.facet-panel__count {
  font-family: var(--tq-font-mono);
  color: var(--tq-text-muted);
  min-width: 30px;
  text-align: right;
}
</style>
