export interface EngineResult {
  p50Ms: number
  p99Ms: number
}

export interface BenchmarkQuery {
  id: string
  name: string
  torque: EngineResult
  typesense: EngineResult
  quickwit: EngineResult | null
  torqueFound: number
  typesenseFound: number
  quickwitFound: number | null
}

export interface ConcurrencyLevel {
  concurrency: number
  queries: BenchmarkQuery[]
  summary: {
    tqWins: number; tsWins: number; geoMean: number
    tqVsQwWins: number; qwGeoMean: number
  }
}

export interface BenchmarkCategory {
  id: string
  label: string
  queryIds: string[]
}

export const benchmarkMeta = {
  dataset: 'bankruptcies_se_20260306161357',
  documents: 309236,
  fields: 67,
  date: '2026-11-06',
  torqueVersion: '0.7.0',
  typesenseVersion: '30.1',
  quickwitVersion: '0.8.2',
  iterations: 100,
  warmup: 5,
}

export const ingestMeta = {
  torque: { docs: 309236, timeS: 6.3, docsPerS: 49396 },
  typesense: { docs: 309236, timeS: 47.9, docsPerS: 6455 },
  quickwit: { docs: 309236, timeS: 7.6, docsPerS: 40476 },
  speedup: 7.7,
}

export const benchmarkCategories: BenchmarkCategory[] = [
  { id: 'text', label: 'Text Search', queryIds: ['text_search', 'text_multi_term', 'prefix_search', 'typo_search'] },
  { id: 'filter', label: 'Filter', queryIds: ['filter_only', 'filter_sort', 'nested_filter', 'multi_filter'] },
  { id: 'facet', label: 'Facet', queryIds: ['faceted', 'text_facet_filter', 'text_facet_sort'] },
  { id: 'sort', label: 'Sort', queryIds: ['sort_only', 'geo_sort'] },
  { id: 'group', label: 'Group By', queryIds: ['group_by_court'] },
  { id: 'combined', label: 'Combined', queryIds: ['text_filter'] },
]

const c1Queries: BenchmarkQuery[] = [
  { id: 'text_search', name: 'Text Search', torque: { p50Ms: 0.68, p99Ms: 0.76 }, typesense: { p50Ms: 2.77, p99Ms: 3.15 }, quickwit: { p50Ms: 3.21, p99Ms: 3.72 }, torqueFound: 12444, typesenseFound: 10173, quickwitFound: 10747 },
  { id: 'text_multi_term', name: 'Multi-Term Text', torque: { p50Ms: 0.73, p99Ms: 0.83 }, typesense: { p50Ms: 2.88, p99Ms: 3.12 }, quickwit: { p50Ms: 3.27, p99Ms: 3.55 }, torqueFound: 1166, typesenseFound: 562, quickwitFound: 24345 },
  { id: 'filter_only', name: 'Filter Only', torque: { p50Ms: 0.15, p99Ms: 0.25 }, typesense: { p50Ms: 1.26, p99Ms: 1.46 }, quickwit: { p50Ms: 3.25, p99Ms: 3.68 }, torqueFound: 7853, typesenseFound: 7853, quickwitFound: 7853 },
  { id: 'filter_sort', name: 'Filter + Sort', torque: { p50Ms: 0.25, p99Ms: 0.29 }, typesense: { p50Ms: 4.93, p99Ms: 5.28 }, quickwit: { p50Ms: 7.82, p99Ms: 9.2 }, torqueFound: 38572, typesenseFound: 38572, quickwitFound: 38572 },
  { id: 'faceted', name: 'Faceted', torque: { p50Ms: 0.16, p99Ms: 0.25 }, typesense: { p50Ms: 8.83, p99Ms: 9.9 }, quickwit: { p50Ms: 3.29, p99Ms: 4.63 }, torqueFound: 309236, typesenseFound: 309236, quickwitFound: 309236 },
  { id: 'text_filter', name: 'Text + Filter', torque: { p50Ms: 0.51, p99Ms: 0.63 }, typesense: { p50Ms: 10.52, p99Ms: 11.04 }, quickwit: { p50Ms: 5.06, p99Ms: 5.55 }, torqueFound: 4574, typesenseFound: 4574, quickwitFound: 2546 },
  { id: 'text_facet_filter', name: 'Text + Facet + Filter', torque: { p50Ms: 0.6, p99Ms: 0.8 }, typesense: { p50Ms: 8.32, p99Ms: 8.73 }, quickwit: { p50Ms: 7.06, p99Ms: 8.66 }, torqueFound: 2793, typesenseFound: 2327, quickwitFound: 2424 },
  { id: 'sort_only', name: 'Sort Only', torque: { p50Ms: 0.15, p99Ms: 0.34 }, typesense: { p50Ms: 6.74, p99Ms: 7.79 }, quickwit: { p50Ms: 7.48, p99Ms: 7.99 }, torqueFound: 309236, typesenseFound: 309236, quickwitFound: 309236 },
  { id: 'prefix_search', name: 'Prefix Search', torque: { p50Ms: 0.2, p99Ms: 0.4 }, typesense: { p50Ms: 1.02, p99Ms: 1.16 }, quickwit: { p50Ms: 7.14, p99Ms: 7.56 }, torqueFound: 914, typesenseFound: 438, quickwitFound: 816 },
  { id: 'typo_search', name: 'Typo Search', torque: { p50Ms: 0.4, p99Ms: 0.47 }, typesense: { p50Ms: 2.23, p99Ms: 2.41 }, quickwit: null, torqueFound: 7465, typesenseFound: 7467, quickwitFound: null },
  { id: 'group_by_court', name: 'Group By Court', torque: { p50Ms: 0.29, p99Ms: 0.4 }, typesense: { p50Ms: 72.43, p99Ms: 75.53 }, quickwit: { p50Ms: 0.85, p99Ms: 0.94 }, torqueFound: 56, typesenseFound: 57, quickwitFound: 309236 },
  { id: 'geo_sort', name: 'Geo Sort', torque: { p50Ms: 0.21, p99Ms: 0.4 }, typesense: { p50Ms: 12.18, p99Ms: 13.04 }, quickwit: null, torqueFound: 105334, typesenseFound: 105334, quickwitFound: null },
  { id: 'nested_filter', name: 'Nested Filter', torque: { p50Ms: 0.16, p99Ms: 0.34 }, typesense: { p50Ms: 1.56, p99Ms: 1.71 }, quickwit: null, torqueFound: 6644, typesenseFound: 6644, quickwitFound: null },
  { id: 'multi_filter', name: 'Multi Filter', torque: { p50Ms: 0.26, p99Ms: 0.33 }, typesense: { p50Ms: 18.07, p99Ms: 19.4 }, quickwit: { p50Ms: 7.44, p99Ms: 7.98 }, torqueFound: 100343, typesenseFound: 100343, quickwitFound: 100343 },
  { id: 'text_facet_sort', name: 'Text + Facet + Sort', torque: { p50Ms: 1.4, p99Ms: 1.58 }, typesense: { p50Ms: 4.81, p99Ms: 5.4 }, quickwit: { p50Ms: 8.05, p99Ms: 9.16 }, torqueFound: 27989, typesenseFound: 14278, quickwitFound: 18057 },
]

const c12Queries: BenchmarkQuery[] = [
  { id: 'text_search', name: 'Text Search', torque: { p50Ms: 1.17, p99Ms: 3.13 }, typesense: { p50Ms: 4.22, p99Ms: 4.94 }, quickwit: { p50Ms: 5.23, p99Ms: 7.13 }, torqueFound: 12444, typesenseFound: 10173, quickwitFound: 10747 },
  { id: 'text_multi_term', name: 'Multi-Term Text', torque: { p50Ms: 1.13, p99Ms: 2.01 }, typesense: { p50Ms: 4.39, p99Ms: 5.42 }, quickwit: { p50Ms: 5.21, p99Ms: 7.74 }, torqueFound: 1166, typesenseFound: 562, quickwitFound: 24345 },
  { id: 'filter_only', name: 'Filter Only', torque: { p50Ms: 0.86, p99Ms: 1.98 }, typesense: { p50Ms: 2.25, p99Ms: 3.8 }, quickwit: { p50Ms: 5.26, p99Ms: 8.01 }, torqueFound: 7853, typesenseFound: 7853, quickwitFound: 7853 },
  { id: 'filter_sort', name: 'Filter + Sort', torque: { p50Ms: 0.81, p99Ms: 2.78 }, typesense: { p50Ms: 7.51, p99Ms: 10.75 }, quickwit: { p50Ms: 13.06, p99Ms: 17.18 }, torqueFound: 38572, typesenseFound: 38572, quickwitFound: 38572 },
  { id: 'faceted', name: 'Faceted', torque: { p50Ms: 0.84, p99Ms: 3.27 }, typesense: { p50Ms: 31.97, p99Ms: 49.18 }, quickwit: { p50Ms: 5.26, p99Ms: 7.27 }, torqueFound: 309236, typesenseFound: 309236, quickwitFound: 309236 },
  { id: 'text_filter', name: 'Text + Filter', torque: { p50Ms: 1.04, p99Ms: 2.27 }, typesense: { p50Ms: 15.37, p99Ms: 17.96 }, quickwit: { p50Ms: 7.86, p99Ms: 9.98 }, torqueFound: 4574, typesenseFound: 4574, quickwitFound: 2546 },
  { id: 'text_facet_filter', name: 'Text + Facet + Filter', torque: { p50Ms: 1.07, p99Ms: 2.24 }, typesense: { p50Ms: 12.59, p99Ms: 15.07 }, quickwit: { p50Ms: 11.11, p99Ms: 17.46 }, torqueFound: 2793, typesenseFound: 2327, quickwitFound: 2424 },
  { id: 'sort_only', name: 'Sort Only', torque: { p50Ms: 0.74, p99Ms: 2.88 }, typesense: { p50Ms: 18.08, p99Ms: 29.23 }, quickwit: { p50Ms: 11.48, p99Ms: 14.43 }, torqueFound: 309236, typesenseFound: 309236, quickwitFound: 309236 },
  { id: 'prefix_search', name: 'Prefix Search', torque: { p50Ms: 0.78, p99Ms: 3.11 }, typesense: { p50Ms: 1.59, p99Ms: 2.19 }, quickwit: { p50Ms: 11.45, p99Ms: 19.31 }, torqueFound: 914, typesenseFound: 438, quickwitFound: 816 },
  { id: 'typo_search', name: 'Typo Search', torque: { p50Ms: 0.92, p99Ms: 2.35 }, typesense: { p50Ms: 3.32, p99Ms: 3.92 }, quickwit: null, torqueFound: 7465, typesenseFound: 7467, quickwitFound: null },
  { id: 'group_by_court', name: 'Group By Court', torque: { p50Ms: 0.82, p99Ms: 2.75 }, typesense: { p50Ms: 160.05, p99Ms: 199.46 }, quickwit: { p50Ms: 4.0, p99Ms: 4.71 }, torqueFound: 56, typesenseFound: 57, quickwitFound: 309236 },
  { id: 'geo_sort', name: 'Geo Sort', torque: { p50Ms: 0.77, p99Ms: 3.13 }, typesense: { p50Ms: 23.68, p99Ms: 30.27 }, quickwit: null, torqueFound: 105334, typesenseFound: 105334, quickwitFound: null },
  { id: 'nested_filter', name: 'Nested Filter', torque: { p50Ms: 0.73, p99Ms: 2.99 }, typesense: { p50Ms: 2.54, p99Ms: 3.78 }, quickwit: null, torqueFound: 6644, typesenseFound: 6644, quickwitFound: null },
  { id: 'multi_filter', name: 'Multi Filter', torque: { p50Ms: 0.8, p99Ms: 2.94 }, typesense: { p50Ms: 29.71, p99Ms: 38.15 }, quickwit: { p50Ms: 11.55, p99Ms: 15.38 }, torqueFound: 100343, typesenseFound: 100343, quickwitFound: 100343 },
  { id: 'text_facet_sort', name: 'Text + Facet + Sort', torque: { p50Ms: 1.96, p99Ms: 3.0 }, typesense: { p50Ms: 9.17, p99Ms: 13.2 }, quickwit: { p50Ms: 13.3, p99Ms: 19.76 }, torqueFound: 27989, typesenseFound: 14278, quickwitFound: 18057 },
]

const c24Queries: BenchmarkQuery[] = [
  { id: 'text_search', name: 'Text Search', torque: { p50Ms: 2.09, p99Ms: 10.2 }, typesense: { p50Ms: 6.9, p99Ms: 18.15 }, quickwit: { p50Ms: 10.19, p99Ms: 15.29 }, torqueFound: 12444, typesenseFound: 10173, quickwitFound: 10747 },
  { id: 'text_multi_term', name: 'Multi-Term Text', torque: { p50Ms: 2.06, p99Ms: 8.2 }, typesense: { p50Ms: 7.55, p99Ms: 15.94 }, quickwit: { p50Ms: 10.36, p99Ms: 13.99 }, torqueFound: 1166, typesenseFound: 562, quickwitFound: 24345 },
  { id: 'filter_only', name: 'Filter Only', torque: { p50Ms: 1.79, p99Ms: 5.84 }, typesense: { p50Ms: 3.93, p99Ms: 10.31 }, quickwit: { p50Ms: 10.19, p99Ms: 14.2 }, torqueFound: 7853, typesenseFound: 7853, quickwitFound: 7853 },
  { id: 'filter_sort', name: 'Filter + Sort', torque: { p50Ms: 1.54, p99Ms: 7.24 }, typesense: { p50Ms: 13.21, p99Ms: 26.38 }, quickwit: { p50Ms: 23.44, p99Ms: 36.31 }, torqueFound: 38572, typesenseFound: 38572, quickwitFound: 38572 },
  { id: 'faceted', name: 'Faceted', torque: { p50Ms: 1.64, p99Ms: 8.19 }, typesense: { p50Ms: 62.17, p99Ms: 108.99 }, quickwit: { p50Ms: 10.17, p99Ms: 16.19 }, torqueFound: 309236, typesenseFound: 309236, quickwitFound: 309236 },
  { id: 'text_filter', name: 'Text + Filter', torque: { p50Ms: 1.86, p99Ms: 6.89 }, typesense: { p50Ms: 23.87, p99Ms: 46.99 }, quickwit: { p50Ms: 13.34, p99Ms: 21.73 }, torqueFound: 4574, typesenseFound: 4574, quickwitFound: 2546 },
  { id: 'text_facet_filter', name: 'Text + Facet + Filter', torque: { p50Ms: 1.91, p99Ms: 6.97 }, typesense: { p50Ms: 21.43, p99Ms: 39.94 }, quickwit: { p50Ms: 18.86, p99Ms: 29.28 }, torqueFound: 2793, typesenseFound: 2327, quickwitFound: 2424 },
  { id: 'sort_only', name: 'Sort Only', torque: { p50Ms: 1.44, p99Ms: 6.6 }, typesense: { p50Ms: 34.07, p99Ms: 66.97 }, quickwit: { p50Ms: 18.93, p99Ms: 29.37 }, torqueFound: 309236, typesenseFound: 309236, quickwitFound: 309236 },
  { id: 'prefix_search', name: 'Prefix Search', torque: { p50Ms: 1.47, p99Ms: 7.58 }, typesense: { p50Ms: 2.65, p99Ms: 7.19 }, quickwit: { p50Ms: 19.2, p99Ms: 29.4 }, torqueFound: 914, typesenseFound: 438, quickwitFound: 816 },
  { id: 'typo_search', name: 'Typo Search', torque: { p50Ms: 1.67, p99Ms: 7.39 }, typesense: { p50Ms: 5.54, p99Ms: 13.5 }, quickwit: null, torqueFound: 7465, typesenseFound: 7467, quickwitFound: null },
  { id: 'group_by_court', name: 'Group By Court', torque: { p50Ms: 1.5, p99Ms: 7.58 }, typesense: { p50Ms: 313.51, p99Ms: 438.92 }, quickwit: { p50Ms: 8.22, p99Ms: 9.22 }, torqueFound: 56, typesenseFound: 57, quickwitFound: 309236 },
  { id: 'geo_sort', name: 'Geo Sort', torque: { p50Ms: 1.54, p99Ms: 7.2 }, typesense: { p50Ms: 44.13, p99Ms: 74.03 }, quickwit: null, torqueFound: 105334, typesenseFound: 105334, quickwitFound: null },
  { id: 'nested_filter', name: 'Nested Filter', torque: { p50Ms: 1.51, p99Ms: 6.53 }, typesense: { p50Ms: 4.32, p99Ms: 11.63 }, quickwit: null, torqueFound: 6644, typesenseFound: 6644, quickwitFound: null },
  { id: 'multi_filter', name: 'Multi Filter', torque: { p50Ms: 1.51, p99Ms: 6.7 }, typesense: { p50Ms: 53.51, p99Ms: 90.99 }, quickwit: { p50Ms: 18.98, p99Ms: 29.76 }, torqueFound: 100343, typesenseFound: 100343, quickwitFound: 100343 },
  { id: 'text_facet_sort', name: 'Text + Facet + Sort', torque: { p50Ms: 3.64, p99Ms: 10.93 }, typesense: { p50Ms: 16.75, p99Ms: 31.27 }, quickwit: { p50Ms: 23.61, p99Ms: 35.47 }, torqueFound: 27989, typesenseFound: 14278, quickwitFound: 18057 },
]

export const concurrencyLevels: ConcurrencyLevel[] = [
  { concurrency: 1, queries: c1Queries, summary: { tqWins: 15, tsWins: 0, geoMean: 16.5, tqVsQwWins: 12, qwGeoMean: 13.3 } },
  { concurrency: 12, queries: c12Queries, summary: { tqWins: 15, tsWins: 0, geoMean: 10.3, tqVsQwWins: 12, qwGeoMean: 8.3 } },
  { concurrency: 24, queries: c24Queries, summary: { tqWins: 15, tsWins: 0, geoMean: 9.5, tqVsQwWins: 12, qwGeoMean: 8.0 } },
]
