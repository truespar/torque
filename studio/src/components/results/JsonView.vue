<template>
  <div class="json-view">
    <div class="json-view__toolbar">
      <button class="json-view__copy-btn" @click="copyToClipboard" :title="copied ? 'Copied!' : 'Copy JSON'">
        <svg v-if="!copied" width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d="M16 1H4c-1.1 0-2 .9-2 2v14h2V3h12V1zm3 4H8c-1.1 0-2 .9-2 2v14c0 1.1.9 2 2 2h11c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2zm0 16H8V7h11v14z"/></svg>
        <svg v-else width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d="M9 16.17L4.83 12l-1.42 1.41L9 19 21 7l-1.41-1.41z"/></svg>
        <span>{{ copied ? 'Copied' : 'Copy' }}</span>
      </button>
    </div>
    <pre class="json-view__content">{{ formatted }}</pre>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'

const props = defineProps<{ data: any }>()
const copied = ref(false)

const formatted = computed(() => JSON.stringify(props.data, null, 2))

async function copyToClipboard() {
  try {
    await navigator.clipboard.writeText(formatted.value)
    copied.value = true
    setTimeout(() => { copied.value = false }, 1500)
  } catch {}
}
</script>

<style scoped>
.json-view {
  height: 100%;
  display: flex;
  flex-direction: column;
  overflow: hidden;
  flex: 1;
}

.json-view__toolbar {
  padding: 6px 12px;
  border-bottom: 1px solid var(--tv-border-default);
  display: flex;
  justify-content: flex-end;
  flex-shrink: 0;
}

.json-view__copy-btn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  padding: 3px 8px;
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-sm);
  background: var(--tv-bg-surface);
  color: var(--tv-text-secondary);
  font-size: 11px;
  font-family: inherit;
  cursor: pointer;
  transition: color 0.15s, border-color 0.15s;
}

.json-view__copy-btn:hover {
  color: var(--tv-text-primary);
  border-color: var(--tv-border-strong);
}

.json-view__content {
  flex: 1;
  overflow: auto;
  padding: 10px 14px;
  font-family: var(--tv-font-mono);
  font-size: var(--tv-font-size-xs);
  color: var(--tv-text-secondary);
  line-height: 1.5;
  margin: 0;
}
</style>
