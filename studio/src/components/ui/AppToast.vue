<template>
  <div class="tq-toast-container">
    <div
      v-for="t in toasts"
      :key="t.id"
      class="tq-toast"
      :class="`tq-toast--${t.type}`"
    >
      {{ t.message }}
    </div>
  </div>
</template>

<script setup lang="ts">
import { useToast } from '@/composables/useToast'
const { toasts } = useToast()
</script>
