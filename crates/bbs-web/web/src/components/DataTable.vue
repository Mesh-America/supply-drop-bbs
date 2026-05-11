<script setup lang="ts" generic="T extends Record<string, any>">
import { computed, ref, watch } from 'vue'

const props = withDefaults(
  defineProps<{
    columns: { key: string; label: string }[]
    rows: T[]
    empty?: string
    rowKey?: (row: T) => string | number
    filterable?: boolean
    filterPlaceholder?: string
    pageSize?: number
  }>(),
  {
    filterable: true,
    filterPlaceholder: 'filter… (substring, any column)',
    pageSize: 0,
  },
)

const filter = ref('')
const page = ref(0)

watch(filter, () => { page.value = 0 })

const filteredRows = computed<T[]>(() => {
  const q = filter.value.trim().toLowerCase()
  if (!q) return props.rows
  return props.rows.filter((row) =>
    props.columns.some((col) => {
      const v = row[col.key]
      if (v === null || v === undefined) return false
      return String(v).toLowerCase().includes(q)
    }),
  )
})

const pageCount = computed(() => {
  if (!props.pageSize) return 1
  return Math.max(1, Math.ceil(filteredRows.value.length / props.pageSize))
})

const visibleRows = computed<T[]>(() => {
  if (!props.pageSize) return filteredRows.value
  const start = page.value * props.pageSize
  return filteredRows.value.slice(start, start + props.pageSize)
})

const pageStart = computed(() => page.value * (props.pageSize || 0) + 1)
const pageEnd = computed(() =>
  Math.min((page.value + 1) * (props.pageSize || 0), filteredRows.value.length),
)

function clearFilter() { filter.value = '' }
function prevPage() { if (page.value > 0) page.value-- }
function nextPage() { if (page.value < pageCount.value - 1) page.value++ }
</script>

<template>
  <div class="datatable">
    <div v-if="filterable" class="filter-bar">
      <input v-model="filter" type="text" class="filter-input" :placeholder="filterPlaceholder" />
      <button v-if="filter" class="secondary clear-btn" @click="clearFilter">clear</button>
      <span class="muted small">
        <template v-if="filter">{{ filteredRows.length }} of {{ rows.length }}</template>
        <template v-else-if="pageSize && rows.length">{{ rows.length }} total</template>
      </span>
    </div>
    <div class="wrap">
      <table v-if="visibleRows.length">
        <thead>
          <tr><th v-for="col in columns" :key="col.key">{{ col.label }}</th></tr>
        </thead>
        <tbody>
          <tr v-for="(row, idx) in visibleRows" :key="rowKey ? rowKey(row) : idx">
            <td v-for="col in columns" :key="col.key">
              <slot :name="`cell:${col.key}`" :row="row" :value="row[col.key]">
                <span v-if="row[col.key] === null || row[col.key] === undefined" class="muted">—</span>
                <template v-else>{{ row[col.key] }}</template>
              </slot>
            </td>
          </tr>
        </tbody>
      </table>
      <p v-else-if="filter && rows.length" class="empty muted">No rows match <code>{{ filter }}</code>.</p>
      <p v-else class="empty muted">{{ empty ?? 'No data.' }}</p>
    </div>
    <div v-if="pageSize && pageCount > 1" class="pagination">
      <button class="secondary small-btn" :disabled="page === 0" @click="prevPage">← prev</button>
      <span class="muted small">{{ pageStart }}–{{ pageEnd }} of {{ filteredRows.length }}</span>
      <button class="secondary small-btn" :disabled="page >= pageCount - 1" @click="nextPage">next →</button>
    </div>
  </div>
</template>

<style scoped>
.datatable { display: flex; flex-direction: column; gap: 0.5rem; }
.filter-bar { display: flex; align-items: center; gap: 0.6rem; }
.filter-input { flex: 1; min-width: 0; max-width: 360px; }
.clear-btn { padding: 0.25rem 0.6rem; font-size: 0.85em; }
.small { font-size: 0.85em; }
.wrap { overflow-x: auto; border: 1px solid var(--border); border-radius: 3px; }
.empty { padding: 1rem; margin: 0; }
.pagination { display: flex; align-items: center; gap: 0.75rem; }
.small-btn { padding: 0.2rem 0.5rem; font-size: 0.8em; }
</style>
