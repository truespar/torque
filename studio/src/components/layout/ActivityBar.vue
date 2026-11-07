<script setup lang="ts">
import { useConnectionStore } from '../../stores/connection'
import { useSettingsStore } from '../../stores/settings'
import type { PanelId } from '../../stores/settings'

const conn = useConnectionStore()
const settings = useSettingsStore()

function toggle(panel: PanelId) {
  settings.activePanel = settings.activePanel === panel ? null : panel
}
</script>

<template>
  <nav class="activity-bar">
    <!-- Logo (click to toggle benchmark) -->
    <button
      class="activity-bar__logo"
      :class="{ 'activity-bar__logo--active': settings.activePanel === 'benchmark' }"
      title="Benchmark"
      @click="toggle('benchmark')"
    >
      <div class="activity-bar__logo-icon">
        <svg viewBox="0 0 512 512" fill="none" xmlns="http://www.w3.org/2000/svg">
          <path d="M227.3 371.862V136.3H277.435L278.989 182.548L269.662 179.05C271.216 171.018 275.621 163.505 282.876 156.509C290.13 149.255 298.94 143.425 309.303 139.021C319.926 134.616 331.067 132.414 342.726 132.414C360.863 132.414 376.927 137.078 390.918 146.405C404.909 155.473 415.92 168.039 423.952 184.103C431.984 199.907 436 218.173 436 238.9C436 259.109 431.984 277.246 423.952 293.309C415.92 309.373 404.909 322.068 390.918 331.396C376.927 340.464 360.992 344.998 343.115 344.998C331.197 344.998 319.797 342.666 308.915 338.002C298.033 333.339 288.835 327.38 281.321 320.125C273.807 312.611 269.014 304.58 266.941 296.03L279.378 290.589V427L227.3 371.862ZM331.844 299.916C342.467 299.916 351.795 297.325 359.827 292.143C368.118 286.961 374.595 279.836 379.259 270.768C383.922 261.441 386.254 250.818 386.254 238.9C386.254 226.723 383.922 216.1 379.259 207.032C374.595 197.705 368.247 190.45 360.215 185.268C352.183 180.087 342.726 177.496 331.844 177.496C321.222 177.496 311.765 180.087 303.474 185.268C295.442 190.45 289.094 197.705 284.43 207.032C279.767 216.1 277.435 226.723 277.435 238.9C277.435 250.818 279.767 261.441 284.43 270.768C289.094 279.836 295.442 286.961 303.474 292.143C311.765 297.325 321.222 299.916 331.844 299.916Z" fill="currentColor"/>
          <path d="M115.253 342.277V213.638V85H166.942V213.638V342.277H115.253ZM76 184.102V137.077H210.47V184.102H76Z" fill="currentColor"/>
        </svg>
      </div>
    </button>

    <!-- Nav icons -->
    <div class="activity-bar__icons">
      <!-- Schema (Database icon) -->
      <button
        class="activity-bar__btn"
        :class="{ 'activity-bar__btn--active': settings.activePanel === 'schema' }"
        title="Schema"
        @click="toggle('schema')"
      >
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <ellipse cx="12" cy="5" rx="9" ry="3" />
          <path d="M3 5V19A9 3 0 0 0 21 19V5" />
          <path d="M3 12A9 3 0 0 0 21 12" />
        </svg>
      </button>

      <!-- API Keys -->
      <button
        class="activity-bar__btn"
        :class="{ 'activity-bar__btn--active': settings.activePanel === 'keys' }"
        title="API Keys"
        @click="toggle('keys')"
      >
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <path d="M2.586 17.414A2 2 0 0 0 2 18.828V21a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h1a1 1 0 0 0 1-1v-1a1 1 0 0 1 1-1h.172a2 2 0 0 0 1.414-.586l.814-.814a6.5 6.5 0 1 0-4-4z" />
          <circle cx="16.5" cy="7.5" r="1.5" fill="currentColor" />
        </svg>
      </button>

      <!-- Import -->
      <button
        class="activity-bar__btn"
        :class="{ 'activity-bar__btn--active': settings.activePanel === 'import' }"
        title="Import"
        @click="toggle('import')"
      >
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <path d="M12 15V3" />
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
          <path d="m7 10 5 5 5-5" />
        </svg>
      </button>

      <!-- Quick Search -->
      <button
        class="activity-bar__btn"
        :class="{ 'activity-bar__btn--active': settings.activePanel === 'quicksearch' }"
        title="Quick Search"
        @click="toggle('quicksearch')"
      >
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <circle cx="11" cy="11" r="8" />
          <path d="m21 21-4.3-4.3" />
        </svg>
      </button>

      <!-- Examples -->
      <button
        class="activity-bar__btn"
        :class="{ 'activity-bar__btn--active': settings.activePanel === 'examples' }"
        title="Examples"
        @click="toggle('examples')"
      >
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <path d="M12.89 1.45l8 4A2 2 0 0 1 22 7.24v9.53a2 2 0 0 1-1.11 1.79l-8 4a2 2 0 0 1-1.79 0l-8-4a2 2 0 0 1-1.1-1.8V7.24a2 2 0 0 1 1.11-1.79l8-4a2 2 0 0 1 1.78 0z" />
          <polyline points="2.32 6.16 12 11 21.68 6.16" />
          <line x1="12" x2="12" y1="22.76" y2="11" />
        </svg>
      </button>

      <!-- Search Reference -->
      <button
        class="activity-bar__btn"
        :class="{ 'activity-bar__btn--active': settings.activePanel === 'reference' }"
        title="Search Reference"
        @click="toggle('reference')"
      >
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20" />
          <path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z" />
        </svg>
      </button>

      <!-- Docs -->
      <button
        class="activity-bar__btn"
        :class="{ 'activity-bar__btn--active': settings.activePanel === 'docs' }"
        title="Docs"
        @click="toggle('docs')"
      >
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <polyline points="4 17 10 11 4 5" />
          <line x1="12" x2="20" y1="19" y2="19" />
        </svg>
      </button>

      <!-- Settings -->
      <button
        class="activity-bar__btn"
        :class="{ 'activity-bar__btn--active': settings.activePanel === 'settings' }"
        title="Settings"
        @click="toggle('settings')"
      >
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
          <circle cx="12" cy="12" r="3" />
        </svg>
      </button>
    </div>

    <!-- Bottom actions -->
    <div class="activity-bar__bottom">
      <!-- Search Assistant, when the server has a model -->
      <button
        v-if="conn.aiChatAvailable"
        class="activity-bar__btn"
        :class="{ 'activity-bar__btn--active': settings.aiChatOpen }"
        title="Search Assistant"
        @click="settings.aiChatOpen = !settings.aiChatOpen"
      >
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">
          <path d="M20.341 6.484A10 10 0 0 1 10.266 21.85" />
          <path d="M3.659 17.516A10 10 0 0 1 13.74 2.152" />
          <circle cx="12" cy="12" r="3" />
          <circle cx="19" cy="5" r="2" />
          <circle cx="5" cy="19" r="2" />
        </svg>
      </button>
    </div>
  </nav>
</template>

<style scoped>
.activity-bar {
  display: flex;
  flex-direction: column;
  width: var(--tv-activitybar-width);
  height: 100%;
  background: var(--tv-bg-surface);
  border-right: 1px solid var(--tv-border-default);
  flex-shrink: 0;
  box-sizing: content-box;
}

.activity-bar__logo {
  display: flex;
  align-items: center;
  justify-content: center;
  height: var(--tv-header-height);
  border: none;
  border-bottom: 1px solid var(--tv-border-default);
  background: transparent;
  flex-shrink: 0;
  cursor: pointer;
  transition: background 0.15s;
}
.activity-bar__logo:hover {
  background: var(--tv-bg-hover);
}
.activity-bar__logo--active {
  background: var(--tv-accent-subtle);
}

.activity-bar__logo-icon {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border-radius: var(--tv-radius-sm);
  background: var(--tv-accent);
  color: var(--tv-text-inverse);
}
.activity-bar__logo-icon svg {
  width: 18px;
  height: 18px;
}

.activity-bar__icons {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 2px;
  padding: 8px 0;
}

.activity-bar__btn {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 40px;
  height: 40px;
  border: none;
  border-radius: var(--tv-radius-sm);
  background: transparent;
  color: var(--tv-text-muted);
  cursor: pointer;
  transition: color 0.15s, background 0.15s;
  position: relative;
}

.activity-bar__btn:hover {
  color: var(--tv-text-primary);
  background: var(--tv-bg-hover);
}

.activity-bar__btn--active {
  color: var(--tv-text-primary);
  background: var(--tv-accent-subtle);
}

.activity-bar__btn--active::before {
  content: '';
  position: absolute;
  left: 0;
  top: 6px;
  bottom: 6px;
  width: 2px;
  background: var(--tv-accent);
  border-radius: 0 1px 1px 0;
}

.activity-bar__bottom {
  margin-top: auto;
  display: flex;
  flex-direction: column;
  align-items: center;
  padding-bottom: 8px;
}
</style>
