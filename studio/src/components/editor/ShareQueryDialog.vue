<script setup lang="ts">
import { computed, ref, onMounted, nextTick } from 'vue'
import { useConnectionStore } from '@/stores/connection'
import { useToast } from '@/composables/useToast'
import { shareUrl } from '@/utils/share'

const props = defineProps<{
  query: string
}>()

const emit = defineEmits<{
  close: []
}>()

const conn = useConnectionStore()
const toast = useToast()

const url = computed(() => shareUrl(props.query, conn.currentCollection))
const urlInput = ref<HTMLInputElement | null>(null)

onMounted(() => {
  nextTick(() => urlInput.value?.focus())
})

async function copyUrl() {
  try {
    await navigator.clipboard.writeText(url.value)
    toast.success('URL copied')
  } catch {
    toast.error('Copy failed')
  }
}

function onKeydown(e: KeyboardEvent) {
  if (e.key === 'Escape') {
    e.preventDefault()
    emit('close')
  }
}
</script>

<template>
  <div class="share-dialog" @keydown="onKeydown">
    <div class="share-dialog__row">
      <input
        ref="urlInput"
        class="share-dialog__input share-dialog__url fg-input"
        :value="url"
        readonly
        @focus="($event.target as HTMLInputElement).select()"
      />
      <button class="share-dialog__btn fg-btn fg-btn--primary" @click="copyUrl">Copy</button>
      <button class="share-dialog__btn fg-btn" @click="$emit('close')">Close</button>
    </div>
  </div>
</template>

<style scoped>
.share-dialog {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 10px 12px;
  background: var(--tv-bg-elevated);
  border-top: 1px solid var(--tv-border-default);
}

.share-dialog__row {
  display: flex;
  align-items: center;
  gap: 8px;
}

.share-dialog__input {
  flex: 1;
  min-width: 0;
  font-size: var(--tv-font-size-sm);
}

.share-dialog__url {
  font-family: var(--tv-font-mono);
  font-size: var(--tv-font-size-xs);
}

.share-dialog__btn {
  flex-shrink: 0;
  font-size: var(--tv-font-size-xs);
  padding: 4px 12px;
  height: 28px;
}
</style>
