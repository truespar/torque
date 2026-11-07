<script setup lang="ts">
import { ref, computed, watch, nextTick } from 'vue'
import { useSettingsStore } from '../../stores/settings'
import {
  benchmarkMeta,
  benchmarkCategories,
  concurrencyLevels,
  ingestMeta,
  type BenchmarkQuery,
} from '../../data/benchmark-data'
import {
  Chart as ChartJS,
  CategoryScale,
  LinearScale,
  LogarithmicScale,
  BarElement,
  Title,
  Tooltip,
  Legend,
} from 'chart.js'
import { Bar } from 'vue-chartjs'

ChartJS.register(CategoryScale, LinearScale, LogarithmicScale, BarElement, Title, Tooltip, Legend)

const settings = useSettingsStore()

// Benchmark state
const showMethodology = ref(false)
const activeCategory = ref(benchmarkCategories[0]?.id ?? 'text')
const activeConcurrency = ref(1)
const selectedQueryId = ref<string | null>(null)

const activeConcLevel = computed(() =>
  concurrencyLevels.find(c => c.concurrency === activeConcurrency.value) ?? concurrencyLevels[0]
)

const activeBenchmarks = computed(() => {
  const cat = benchmarkCategories.find(c => c.id === activeCategory.value)
  if (!cat) return []
  return activeConcLevel.value.queries.filter(q => cat.queryIds.includes(q.id))
})

const maxQps = computed(() => {
  if (!activeBenchmarks.value.length) return 1
  return Math.max(
    ...activeBenchmarks.value.flatMap(q => {
      const vals = [1000 / q.torque.p50Ms, 1000 / q.typesense.p50Ms]
      if (q.quickwit?.p50Ms) vals.push(1000 / q.quickwit.p50Ms)
      return vals
    })
  )
})

function barWidth(p50Ms: number): string {
  const qps = 1000 / p50Ms
  return `${Math.max((qps / maxQps.value) * 100, 2)}%`
}

function formatQps(p50Ms: number): string {
  const qps = 1000 / p50Ms
  if (qps >= 1_000_000) return `${(qps / 1_000_000).toFixed(1)}M`
  if (qps >= 1_000) return `${(qps / 1_000).toFixed(1)}K`
  if (qps >= 1) return String(Math.round(qps))
  return qps.toFixed(2)
}

function formatLatency(ms: number): string {
  if (ms >= 1000) return `${(ms / 1000).toFixed(1)}s`
  if (ms >= 100) return `${Math.round(ms)}ms`
  if (ms >= 10) return `${ms.toFixed(1)}ms`
  return `${ms.toFixed(1)}ms`
}

function speedupRatio(tqP50: number, tsP50: number): string {
  if (!tqP50) return '-'
  const r = tsP50 / tqP50
  if (r >= 10) return `${Math.round(r)}x`
  return `${r.toFixed(1)}x`
}

function isFast(q: BenchmarkQuery): boolean {
  return q.typesense.p50Ms > q.torque.p50Ms * 1.1
}

function selectQuery(id: string) {
  selectedQueryId.value = selectedQueryId.value === id ? null : id
}

const scalingQuery = computed(() => {
  if (selectedQueryId.value) {
    const q = activeConcLevel.value.queries.find(q => q.id === selectedQueryId.value)
    if (q) return q
  }
  const cat = benchmarkCategories.find(c => c.id === activeCategory.value)
  if (!cat || !cat.queryIds.length) return null
  return activeConcLevel.value.queries.find(q => q.id === cat.queryIds[0]) ?? null
})

// Chart.js data for the scaling chart
const chartKey = ref(0)

function getCssVar(name: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim()
}

const chartData = computed(() => {
  void chartKey.value
  const qId = scalingQuery.value?.id
  if (!qId) return { labels: [], datasets: [] }

  const labels = concurrencyLevels.map(c => c.concurrency === 1 ? '1 client' : `${c.concurrency} clients`)
  const tqData: number[] = []
  const tsData: number[] = []
  const tvData: number[] = []

  for (const cl of concurrencyLevels) {
    const q = cl.queries.find(q => q.id === qId)
    tqData.push(q ? 1000 / q.torque.p50Ms : 0)
    tsData.push(q ? 1000 / q.typesense.p50Ms : 0)
    tvData.push(q?.quickwit?.p50Ms ? 1000 / q.quickwit.p50Ms : 0)
  }

  return {
    labels,
    datasets: [
      {
        label: 'Torque',
        data: tqData,
        backgroundColor: getCssVar('--tv-accent') || '#0EA5E9',
        borderRadius: 3,
        barPercentage: 0.95,
        categoryPercentage: 0.8,
      },
      {
        label: 'Typesense',
        data: tsData,
        backgroundColor: getCssVar('--tv-competitor') || '#E8716E',
        borderRadius: 3,
        barPercentage: 0.95,
        categoryPercentage: 0.8,
      },
      {
        label: 'Quickwit',
        data: tvData,
        backgroundColor: getCssVar('--tv-status-success') || '#22C55E',
        borderRadius: 3,
        barPercentage: 0.95,
        categoryPercentage: 0.8,
      },
    ],
  }
})

const chartOptions = computed(() => {
  void chartKey.value
  const textColor = getCssVar('--tv-text-muted') || '#888'
  const gridColor = getCssVar('--tv-border-subtle') || 'rgba(128,128,128,0.15)'

  return {
    responsive: true,
    maintainAspectRatio: false,
    animation: false as const,
    layout: { padding: { top: 8 } },
    plugins: {
      legend: { display: false },
      tooltip: {
        backgroundColor: getCssVar('--tv-bg-elevated') || '#1A2834',
        titleColor: getCssVar('--tv-text-primary') || '#ECE8E0',
        bodyColor: getCssVar('--tv-text-secondary') || '#7E8C9C',
        borderColor: getCssVar('--tv-border-default') || '#2A3A48',
        borderWidth: 1,
        padding: 10,
        cornerRadius: 6,
        callbacks: {
          label(ctx: any) {
            const v = ctx.parsed.y
            if (v == null) return `${ctx.dataset.label}: N/A`
            return `${ctx.dataset.label}: ${Math.round(v)} queries/sec`
          },
        },
      },
    },
    scales: {
      x: {
        ticks: { color: textColor, font: { size: 12 } },
        grid: { display: false },
        border: { display: false },
      },
      y: {
        type: 'linear' as const,
        beginAtZero: true,
        ticks: {
          color: textColor,
          font: { size: 11 },
          callback(value: any) { return `${Math.round(Number(value))}` },
        },
        grid: { color: gridColor },
        border: { display: false },
        title: {
          display: true,
          text: 'Queries / sec (higher is better)',
          color: textColor,
          font: { size: 11 },
        },
      },
    },
  }
})

const chartOverlayPlugin = {
  id: 'chartOverlay',
  afterDatasetsDraw(chart: any) {
    const { ctx } = chart
    const tqMeta = chart.getDatasetMeta(0)
    if (!tqMeta.visible) return
    const inverseFill = getCssVar('--tv-text-inverse') || '#fff'
    const textColor = getCssVar('--tv-text-secondary') || '#888'

    // Draw engine name inside each bar
    for (let dsIdx = 0; dsIdx < chart.data.datasets.length; dsIdx++) {
      const meta = chart.getDatasetMeta(dsIdx)
      if (!meta.visible) continue
      const ds = chart.data.datasets[dsIdx]

      for (let i = 0; i < meta.data.length; i++) {
        const el = meta.data[i]
        const val = ds.data[i] as number | null
        if (!val || val <= 0) continue

        const barHeight = el.base - el.y
        if (barHeight < 18) continue

        ctx.save()
        ctx.fillStyle = inverseFill
        ctx.font = 'bold 10px sans-serif'
        ctx.textAlign = 'center'
        ctx.textBaseline = 'top'
        ctx.fillText(ds.label, el.x, el.y + 4)
        ctx.restore()
      }
    }

    // Draw speedup annotations on competitor bars
    for (let dsIdx = 1; dsIdx < chart.data.datasets.length; dsIdx++) {
      const compMeta = chart.getDatasetMeta(dsIdx)
      if (!compMeta.visible) continue

      for (let i = 0; i < compMeta.data.length; i++) {
        const tqEl = tqMeta.data[i]
        const compEl = compMeta.data[i]
        const tqVal = chart.data.datasets[0].data[i] as number
        const compVal = chart.data.datasets[dsIdx].data[i] as number
        if (!tqVal || !compVal) continue

        const ratio = tqVal / compVal
        if (ratio < 1.15) continue

        const yBot = compEl.y
        const yTop = tqEl.y
        const gap = yBot - yTop
        if (gap < 12) continue

        const x = compEl.x
      ctx.save()

      ctx.setLineDash([4, 4])
      ctx.strokeStyle = textColor
      ctx.globalAlpha = 0.5
      ctx.lineWidth = 1.5
      ctx.beginPath()
      ctx.moveTo(x, yBot + 2)
      ctx.lineTo(x, yTop + 10)
      ctx.stroke()

      ctx.setLineDash([])
      ctx.globalAlpha = 0.6
      ctx.fillStyle = textColor
      ctx.beginPath()
      ctx.moveTo(x, yTop + 3)
      ctx.lineTo(x - 4, yTop + 10)
      ctx.lineTo(x + 4, yTop + 10)
      ctx.closePath()
      ctx.fill()

      ctx.globalAlpha = 1
      ctx.fillStyle = textColor
      ctx.font = 'bold 13px monospace'
      ctx.textAlign = 'left'
      ctx.textBaseline = 'middle'
      const label = ratio >= 10 ? `${Math.round(ratio)}x` : `${ratio.toFixed(1)}x`
      ctx.fillText(label, x + 8, yTop + gap / 2)

      ctx.restore()
      }
    }
  },
}

const chartPlugins = [chartOverlayPlugin]

watch(() => settings.theme, () => {
  nextTick(() => { chartKey.value++ })
})
</script>

<template>
  <div class="benchmark">
    <div class="benchmark__scroll">
      <div class="benchmark__content">
        <!-- Hero -->
        <section class="benchmark__hero">
          <div class="benchmark__hero-logo">
            <svg viewBox="0 0 512 512" fill="none" xmlns="http://www.w3.org/2000/svg">
              <path d="M227.3 371.862V136.3H277.435L278.989 182.548L269.662 179.05C271.216 171.018 275.621 163.505 282.876 156.509C290.13 149.255 298.94 143.425 309.303 139.021C319.926 134.616 331.067 132.414 342.726 132.414C360.863 132.414 376.927 137.078 390.918 146.405C404.909 155.473 415.92 168.039 423.952 184.103C431.984 199.907 436 218.173 436 238.9C436 259.109 431.984 277.246 423.952 293.309C415.92 309.373 404.909 322.068 390.918 331.396C376.927 340.464 360.992 344.998 343.115 344.998C331.197 344.998 319.797 342.666 308.915 338.002C298.033 333.339 288.835 327.38 281.321 320.125C273.807 312.611 269.014 304.58 266.941 296.03L279.378 290.589V427L227.3 371.862ZM331.844 299.916C342.467 299.916 351.795 297.325 359.827 292.143C368.118 286.961 374.595 279.836 379.259 270.768C383.922 261.441 386.254 250.818 386.254 238.9C386.254 226.723 383.922 216.1 379.259 207.032C374.595 197.705 368.247 190.45 360.215 185.268C352.183 180.087 342.726 177.496 331.844 177.496C321.222 177.496 311.765 180.087 303.474 185.268C295.442 190.45 289.094 197.705 284.43 207.032C279.767 216.1 277.435 226.723 277.435 238.9C277.435 250.818 279.767 261.441 284.43 270.768C289.094 279.836 295.442 286.961 303.474 292.143C311.765 297.325 321.222 299.916 331.844 299.916Z" fill="currentColor"/>
              <path d="M115.253 342.277V213.638V85H166.942V213.638V342.277H115.253ZM76 184.102V137.077H210.47V184.102H76Z" fill="currentColor"/>
            </svg>
          </div>
          <h1 class="benchmark__hero-title">Torque</h1>
          <p class="benchmark__hero-tagline">In-memory search engine for the modern data stack</p>

          <div class="benchmark__pills">
            <div class="benchmark__pill">
              <span class="benchmark__pill-value">{{ activeConcLevel.summary.geoMean }}x</span>
              <span class="benchmark__pill-label">faster than Typesense</span>
            </div>
            <div class="benchmark__pill">
              <span class="benchmark__pill-value">{{ activeConcLevel.summary.qwGeoMean }}x</span>
              <span class="benchmark__pill-label">faster than Quickwit</span>
            </div>
            <div class="benchmark__pill">
              <span class="benchmark__pill-value">{{ ingestMeta.speedup }}x</span>
              <span class="benchmark__pill-label">faster ingest</span>
            </div>
            <div class="benchmark__pill">
              <span class="benchmark__pill-value">{{ (benchmarkMeta.documents / 1000).toFixed(0) }}K</span>
              <span class="benchmark__pill-label">documents, {{ benchmarkMeta.fields }} fields</span>
            </div>
          </div>
        </section>

        <!-- Benchmark -->
        <section class="benchmark__section">
          <h2 class="benchmark__section-title">
            Benchmark
            <button class="benchmark__method-link" @click="showMethodology = true">Methodology</button>
          </h2>

          <div class="benchmark__controls">
            <div class="benchmark__tabs">
              <button
                v-for="cat in benchmarkCategories"
                :key="cat.id"
                class="benchmark__tab"
                :class="{ 'benchmark__tab--active': activeCategory === cat.id }"
                @click="activeCategory = cat.id"
              >{{ cat.label }}</button>
            </div>

            <div class="benchmark__conc">
              <span class="benchmark__conc-label">Parallel clients:</span>
              <div class="benchmark__conc-pills">
                <button
                  v-for="cl in concurrencyLevels"
                  :key="cl.concurrency"
                  class="benchmark__conc-pill"
                  :class="{ 'benchmark__conc-pill--active': activeConcurrency === cl.concurrency }"
                  @click="activeConcurrency = cl.concurrency"
                >{{ cl.concurrency }}</button>
              </div>
            </div>
          </div>

          <div v-if="activeBenchmarks.length" class="benchmark__rows">
            <div
              v-for="q in activeBenchmarks"
              :key="q.id"
              class="benchmark__row"
              :class="{ 'benchmark__row--selected': selectedQueryId === q.id }"
              @click="selectQuery(q.id)"
            >
              <span class="benchmark__name">{{ q.name }}</span>
              <div class="benchmark__bars">
                <div class="benchmark__bar-group">
                  <div class="benchmark__bar-row">
                    <div class="benchmark__bar benchmark__bar--torque" :style="{ width: barWidth(q.torque.p50Ms) }">
                      <span class="benchmark__bar-label">Torque</span>
                      <span class="benchmark__bar-value">{{ formatQps(q.torque.p50Ms) }}</span>
                    </div>
                    <svg v-if="isFast(q)" class="benchmark__rabbit" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                      <path d="M13 16a3 3 0 0 1 2.24 5" /><path d="M18 12h.01" /><path d="M18 21h-8a4 4 0 0 1-4-4 7 7 0 0 1 7-7h.2L9.6 6.4a1 1 0 1 1 2.8-2.8L15.8 7h.2c3.3 0 6 2.7 6 6v1a2 2 0 0 1-2 2h-1a3 3 0 0 0-3 3" /><path d="M20 8.54V4a2 2 0 1 0-4 0v3" /><path d="M7.612 12.524a3 3 0 1 0-1.6 4.3" />
                    </svg>
                  </div>
                  <div class="benchmark__bar benchmark__bar--typesense" :style="{ width: barWidth(q.typesense.p50Ms) }">
                    <span class="benchmark__bar-label">Typesense</span>
                    <span class="benchmark__bar-value">{{ formatQps(q.typesense.p50Ms) }}</span>
                  </div>
                  <div v-if="q.quickwit" class="benchmark__bar benchmark__bar--quickwit" :style="{ width: barWidth(q.quickwit.p50Ms) }">
                    <span class="benchmark__bar-label">Quickwit</span>
                    <span class="benchmark__bar-value">{{ formatQps(q.quickwit.p50Ms) }}</span>
                  </div>
                  <div v-else-if="q.quickwit === null" class="benchmark__bar-na">
                    <span class="benchmark__bar-na-text">Quickwit &mdash; N/A</span>
                  </div>
                </div>
                <div class="benchmark__ratios">
                  <span class="benchmark__ratio">
                    <span class="benchmark__ratio-dot benchmark__ratio-dot--typesense"></span>
                    {{ speedupRatio(q.torque.p50Ms, q.typesense.p50Ms) }}
                  </span>
                  <span v-if="q.quickwit?.p50Ms" class="benchmark__ratio benchmark__ratio--quickwit">
                    <span class="benchmark__ratio-dot benchmark__ratio-dot--quickwit"></span>
                    {{ speedupRatio(q.torque.p50Ms, q.quickwit.p50Ms) }}
                  </span>
                  <span v-else-if="q.quickwit === null" class="benchmark__ratio benchmark__ratio--na">&mdash;</span>
                </div>
              </div>
            </div>

            <!-- Summary -->
            <div class="benchmark__summary">
              <span>
                <span class="benchmark__ratio-dot benchmark__ratio-dot--typesense"></span>
                Torque wins {{ activeConcLevel.summary.tqWins }}/{{ activeConcLevel.summary.tqWins + activeConcLevel.summary.tsWins }}
                vs Typesense &middot; {{ activeConcLevel.summary.geoMean }}x avg speedup
              </span>
              <span class="benchmark__summary-sep">|</span>
              <span>
                <span class="benchmark__ratio-dot benchmark__ratio-dot--quickwit"></span>
                Torque wins {{ activeConcLevel.summary.tqVsQwWins }}/{{ activeConcLevel.queries.filter(q => q.quickwit !== null).length }}
                vs Quickwit &middot; {{ activeConcLevel.summary.qwGeoMean }}x avg
              </span>
            </div>
          </div>

          <!-- Scaling chart -->
          <div v-if="scalingQuery" class="benchmark__chart-section">
            <h3 class="benchmark__chart-title">
              Throughput by concurrency: {{ scalingQuery.name }}
            </h3>
            <div class="benchmark__chart-container">
              <Bar :key="chartKey" :data="chartData" :options="chartOptions" :plugins="chartPlugins" />
            </div>
          </div>
        </section>
      </div>
    </div>

    <!-- Methodology modal -->
    <Teleport to="body">
      <div v-if="showMethodology" class="benchmark__modal-overlay" @click.self="showMethodology = false">
        <div class="benchmark__modal">
          <div class="benchmark__modal-header">
            <h3 class="benchmark__modal-title">Benchmark Methodology</h3>
            <button class="benchmark__modal-close" @click="showMethodology = false">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>
          <div class="benchmark__modal-body">
            <table class="benchmark__modal-table">
              <tr><td>Dataset</td><td>Swedish Bankruptcies &mdash; {{ (benchmarkMeta.documents / 1000).toFixed(0) }}K documents, {{ benchmarkMeta.fields }} fields</td></tr>
              <tr><td>Torque</td><td>v{{ benchmarkMeta.torqueVersion }}</td></tr>
              <tr><td>Typesense</td><td>v{{ benchmarkMeta.typesenseVersion }}</td></tr>
              <tr><td>Quickwit</td><td>v{{ benchmarkMeta.quickwitVersion }} (Elasticsearch-compatible HTTP API)</td></tr>
              <tr><td>Date</td><td>{{ benchmarkMeta.date }}</td></tr>
              <tr><td>CPU</td><td>Intel Core i7-12700KF; engines and client pinned to its 8 P-cores (16 threads, 4.9 GHz), performance governor</td></tr>
              <tr><td>RAM</td><td>64 GB DDR4</td></tr>
              <tr><td>OS</td><td>Ubuntu 24.04 LTS, kernel 7.0</td></tr>
              <tr><td>Duration</td><td>{{ benchmarkMeta.iterations }} iterations per query, {{ benchmarkMeta.warmup }} warmup</td></tr>
              <tr><td>Concurrency</td><td>1, 12, and 24 concurrent clients</td></tr>
              <tr><td>Fresh data</td><td>All engines reimported before each run</td></tr>
              <tr><td>Ingest</td><td>Until every document is searchable &mdash; Torque: TCP binary ({{ (ingestMeta.torque.docsPerS / 1000).toFixed(0) }}K docs/s) &mdash; Typesense: HTTP JSONL ({{ (ingestMeta.typesense.docsPerS / 1000).toFixed(0) }}K docs/s) &mdash; Quickwit: HTTP NDJSON ({{ (ingestMeta.quickwit.docsPerS / 1000).toFixed(0) }}K docs/s)</td></tr>
            </table>
            <p class="benchmark__modal-note">
              All three engines ran the same queries via HTTP on localhost for an apples-to-apples comparison.
              Quickwit (built on Tantivy) was queried via its native Elasticsearch-compatible REST API.
              Quickwit does not support fuzzy/typo search, nested document filtering, or geo distance
              sorting (marked N/A); its prefix search is a <code>match_phrase_prefix</code> query. The dataset contains nested objects, arrays, geo coordinates,
              and financial data. Text search differences in <code>found</code> counts reflect different
              stemming/tokenization between engines.
            </p>
          </div>
        </div>
      </div>
    </Teleport>
  </div>
</template>

<style scoped>
.benchmark {
  width: 100%;
  height: 100%;
  overflow: hidden;
  background: var(--tv-bg-base);
}

.benchmark__scroll {
  height: 100%;
  overflow-y: auto;
  scrollbar-width: thin;
  scrollbar-color: var(--tv-border-default) transparent;
}

.benchmark__content {
  max-width: 960px;
  margin: 0 auto;
  padding: 48px 32px 64px;
}

/* Hero */
.benchmark__hero {
  display: flex;
  flex-direction: column;
  align-items: center;
  text-align: center;
  padding-bottom: 40px;
  border-bottom: 1px solid var(--tv-border-default);
  margin-bottom: 40px;
}

.benchmark__hero-logo {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 48px;
  height: 48px;
  border-radius: var(--tv-radius-lg);
  background: var(--tv-accent);
  color: var(--tv-text-inverse);
  margin-bottom: 16px;
}
.benchmark__hero-logo svg {
  width: 30px;
  height: 30px;
}

.benchmark__hero-title {
  font-size: 24px;
  font-weight: 700;
  color: var(--tv-text-primary);
  margin: 0 0 6px;
  letter-spacing: -0.02em;
}

.benchmark__hero-tagline {
  font-size: 15px;
  color: var(--tv-text-secondary);
  margin: 0 0 24px;
}

.benchmark__pills {
  display: flex;
  gap: 12px;
  flex-wrap: wrap;
  justify-content: center;
}

.benchmark__pill {
  display: flex;
  align-items: baseline;
  gap: 6px;
  padding: 6px 14px;
  background: var(--tv-bg-elevated);
  border-radius: 20px;
  border: 1px solid var(--tv-border-default);
}

.benchmark__pill-value {
  font-size: 15px;
  font-weight: 700;
  color: var(--tv-accent);
  font-family: var(--tv-font-mono);
}

.benchmark__pill-label {
  font-size: 12px;
  color: var(--tv-text-muted);
}

/* Sections */
.benchmark__section {
  margin-bottom: 40px;
}

.benchmark__section-title {
  display: flex;
  align-items: center;
  gap: 12px;
  font-size: 14px;
  font-weight: 600;
  color: var(--tv-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.06em;
  margin: 0 0 16px;
}

.benchmark__method-link {
  border: none;
  background: none;
  color: var(--tv-accent);
  font-size: 11px;
  font-weight: 600;
  font-family: var(--tv-font-sans);
  text-transform: uppercase;
  letter-spacing: 0.04em;
  cursor: pointer;
  opacity: 0.8;
  transition: opacity 0.15s;
}
.benchmark__method-link:hover {
  opacity: 1;
}

/* Controls */
.benchmark__controls {
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin-bottom: 16px;
}

.benchmark__tabs {
  display: flex;
  gap: 2px;
  background: var(--tv-bg-surface);
  border-radius: var(--tv-radius-md);
  padding: 3px;
  border: 1px solid var(--tv-border-default);
  width: fit-content;
}

.benchmark__tab {
  padding: 5px 14px;
  border: none;
  border-radius: var(--tv-radius-sm);
  background: transparent;
  color: var(--tv-text-secondary);
  font-size: 12px;
  font-weight: 500;
  font-family: var(--tv-font-sans);
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}
.benchmark__tab:hover {
  color: var(--tv-text-primary);
  background: var(--tv-bg-hover);
}
.benchmark__tab--active {
  background: var(--tv-accent-subtle);
  color: var(--tv-accent);
}

.benchmark__conc {
  display: flex;
  align-items: center;
  gap: 8px;
}

.benchmark__conc-label {
  font-size: 12px;
  color: var(--tv-text-muted);
}

.benchmark__conc-pills {
  display: flex;
  gap: 2px;
  background: var(--tv-bg-surface);
  border-radius: var(--tv-radius-md);
  padding: 3px;
  border: 1px solid var(--tv-border-default);
}

.benchmark__conc-pill {
  padding: 4px 12px;
  border: none;
  border-radius: var(--tv-radius-sm);
  background: transparent;
  color: var(--tv-text-secondary);
  font-size: 12px;
  font-weight: 600;
  font-family: var(--tv-font-mono);
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}
.benchmark__conc-pill:hover {
  color: var(--tv-text-primary);
  background: var(--tv-bg-hover);
}
.benchmark__conc-pill--active {
  background: var(--tv-accent-subtle);
  color: var(--tv-accent);
}

/* Benchmark rows */
.benchmark__rows {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.benchmark__row {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 6px 8px;
  border-radius: var(--tv-radius-sm);
  cursor: pointer;
  transition: background 0.1s;
}
.benchmark__row:hover {
  background: var(--tv-bg-hover);
}
.benchmark__row--selected {
  background: var(--tv-accent-subtle);
}

.benchmark__name {
  font-size: 12px;
  font-weight: 500;
  color: var(--tv-text-secondary);
  font-family: var(--tv-font-mono);
}

.benchmark__bars {
  display: flex;
  align-items: center;
  gap: 12px;
}

.benchmark__bar-group {
  flex: 1;
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.benchmark__bar-row {
  display: flex;
  align-items: center;
  gap: 6px;
}

.benchmark__bar {
  height: 20px;
  border-radius: 3px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 8px;
  min-width: 80px;
  transition: width 0.3s;
  overflow: hidden;
  white-space: nowrap;
}

.benchmark__bar--torque {
  background: var(--tv-accent);
}

.benchmark__bar--typesense {
  background: var(--tv-competitor);
}

.benchmark__bar--quickwit {
  background: var(--tv-status-success);
}

.benchmark__bar-na {
  height: 18px;
  display: flex;
  align-items: center;
  padding: 0 8px;
}

.benchmark__bar-na-text {
  font-size: 10px;
  color: var(--tv-text-muted);
  font-family: var(--tv-font-mono);
}

.benchmark__ratio-dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  border-radius: 50%;
  margin-right: 2px;
}

.benchmark__ratio-dot--typesense {
  background: var(--tv-competitor);
}

.benchmark__ratio-dot--quickwit {
  background: var(--tv-status-success);
}

.benchmark__ratio--quickwit {
  color: var(--tv-status-success);
}

.benchmark__ratio--na {
  color: var(--tv-text-muted);
}

.benchmark__summary-sep {
  color: var(--tv-text-muted);
}

.benchmark__bar-label {
  font-size: 10px;
  font-weight: 700;
  color: var(--tv-text-inverse);
  font-family: var(--tv-font-sans);
}

.benchmark__bar-value {
  font-size: 10px;
  font-weight: 700;
  color: var(--tv-text-inverse);
  font-family: var(--tv-font-mono);
}

.benchmark__rabbit {
  flex-shrink: 0;
  color: var(--tv-accent);
  animation: benchmark-bounce 2s ease-in-out infinite;
}

@keyframes benchmark-bounce {
  0%, 100% { transform: translateX(0); }
  50% { transform: translateX(4px); }
}

.benchmark__ratios {
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 1px;
  min-width: 60px;
}

.benchmark__ratio {
  display: flex;
  align-items: center;
  gap: 4px;
  font-size: 12px;
  font-weight: 700;
  font-family: var(--tv-font-mono);
  color: var(--tv-accent);
}

/* Summary */
.benchmark__summary {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  margin-top: 8px;
  padding: 8px 12px;
  background: var(--tv-bg-surface);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  font-size: 12px;
  font-family: var(--tv-font-mono);
  color: var(--tv-text-secondary);
}

/* Chart */
.benchmark__chart-section {
  margin-top: 24px;
}

.benchmark__chart-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--tv-text-secondary);
  font-family: var(--tv-font-mono);
  margin: 0 0 8px;
}

.benchmark__chart-container {
  height: 360px;
  background: var(--tv-bg-surface);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-md);
  padding: 16px;
}

/* Methodology modal */
.benchmark__modal-overlay {
  position: fixed;
  inset: 0;
  z-index: 9999;
  display: flex;
  align-items: center;
  justify-content: center;
  background: var(--tv-bg-overlay);
}

.benchmark__modal {
  width: 580px;
  max-width: 90vw;
  max-height: 80vh;
  background: var(--tv-bg-surface);
  border: 1px solid var(--tv-border-default);
  border-radius: var(--tv-radius-lg);
  box-shadow: 0 25px 50px -12px rgba(0, 0, 0, 0.5);
  display: flex;
  flex-direction: column;
}

.benchmark__modal-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 16px 20px;
  border-bottom: 1px solid var(--tv-border-default);
}

.benchmark__modal-title {
  font-size: 15px;
  font-weight: 700;
  color: var(--tv-text-primary);
  margin: 0;
}

.benchmark__modal-close {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 28px;
  height: 28px;
  border: none;
  border-radius: var(--tv-radius-sm);
  background: transparent;
  color: var(--tv-text-muted);
  cursor: pointer;
  transition: background 0.15s, color 0.15s;
}
.benchmark__modal-close:hover {
  background: var(--tv-bg-hover);
  color: var(--tv-text-primary);
}

.benchmark__modal-body {
  padding: 20px;
  overflow-y: auto;
}

.benchmark__modal-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
  margin-bottom: 16px;
}

.benchmark__modal-table td {
  padding: 6px 0;
  border-bottom: 1px solid var(--tv-border-subtle);
  color: var(--tv-text-secondary);
}

.benchmark__modal-table td:first-child {
  font-weight: 600;
  color: var(--tv-text-primary);
  width: 120px;
  white-space: nowrap;
  padding-right: 16px;
}

.benchmark__modal-table code {
  font-family: var(--tv-font-mono);
  font-size: 12px;
  padding: 1px 5px;
  background: var(--tv-bg-elevated);
  border-radius: 3px;
}

.benchmark__modal-note {
  font-size: 12px;
  color: var(--tv-text-muted);
  line-height: 1.6;
  margin: 0;
}
</style>
