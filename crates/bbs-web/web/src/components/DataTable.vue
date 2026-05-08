<script setup lang="ts" generic="T extends Record<string, any>">
import { computed, ref } from 'vue'

const props = withDefaults(
  defineProps<{
    columns: { key: string; label: string }[]
    rows: T[]
    empty?: string
    rowKey?: (row: T) => string | number
    filterable?: boolean
    filterPlaceholder?: string
  }>(),
  {
    filterable: true,
    filterPlaceholder: 'filter… (substring, any column)',
  },
)

const filter = ref('')

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

function clearFilter() { filter.value = '' }
</script>

<template>
  <div class="datatable">
    <div v-if="filterable" class="filter-bar">
      <input v-model="filter" type="text" class="filter-input" :placeholder="filterPlaceholder" />
      <button v-if="filter" class="secondary clear-btn" @click="clearFilter">clear</button>
      <span v-if="filter" class="muted small">{{ filteredRows.length }} of {{ rows.length }}</span>
    </div>
    <div class="wrap">
      <table v-if="filteredRows.length">
        <thead>
          <tr><th v-for="col in columns" :key="col.key">{{ col.label }}</th></tr>
        </thead>
        <tbody>
          <tr v-for="(row, idx) in filteredRows" :key="rowKey ? rowKey(row) : idx">
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
</style>
