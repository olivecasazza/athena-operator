<template>
  <div class="min-h-screen flex flex-col items-center justify-center p-8">
    <header class="mb-8 text-center">
      <h1 class="text-4xl font-bold text-pink-400">Athena Console</h1>
      <p class="text-gray-400 mt-2">Kubernetes Research Operator Dashboard</p>
    </header>

    <main class="w-full max-w-6xl bg-gray-800 p-6 rounded-lg shadow-lg space-y-8">
      <section>
        <div class="mb-6 flex justify-between items-center">
          <div>
            <h2 class="text-2xl font-semibold">Benchmark Runs</h2>
            <p class="text-gray-400 text-sm">Early benchmark API scaffold: phase, suite, target, cost, and report links.</p>
          </div>
          <button @click="refreshAll" class="bg-pink-600 hover:bg-pink-500 px-4 py-2 rounded text-sm transition-colors">
            Refresh
          </button>
        </div>

        <div v-if="loading" class="text-gray-500 text-center py-8">Loading Athena resources...</div>

        <div v-else-if="error" class="bg-red-900/50 border border-red-500 text-red-200 p-4 rounded mb-4">
          {{ error }}
        </div>

        <div v-else-if="benchmarkRuns.length === 0" class="text-gray-500 text-center py-8 border-2 border-dashed border-gray-700 rounded">
          No benchmark runs found in the cluster.
        </div>

        <div v-else class="overflow-x-auto">
          <table class="w-full text-left text-sm">
            <thead class="text-gray-300 border-b border-gray-700">
              <tr>
                <th class="py-2 pr-4">Run</th>
                <th class="py-2 pr-4">Phase</th>
                <th class="py-2 pr-4">Suite</th>
                <th class="py-2 pr-4">Target</th>
                <th class="py-2 pr-4">GPU Hours</th>
                <th class="py-2 pr-4">Report</th>
              </tr>
            </thead>
            <tbody>
              <tr v-for="run in benchmarkRuns" :key="run.metadata.uid" class="border-b border-gray-700/70">
                <td class="py-3 pr-4">
                  <div class="font-medium text-pink-300">{{ run.metadata.name }}</div>
                  <div class="text-xs text-gray-500">{{ run.metadata.namespace }}</div>
                </td>
                <td class="py-3 pr-4">
                  <span class="px-2 py-1 text-xs rounded bg-blue-900 text-blue-200">
                    {{ run.status?.phase || 'Unknown' }}
                  </span>
                </td>
                <td class="py-3 pr-4">{{ run.spec?.suiteRef?.name || '—' }}</td>
                <td class="py-3 pr-4">{{ run.spec?.targetRef?.kind || '—' }}/{{ run.spec?.targetRef?.name || '—' }}</td>
                <td class="py-3 pr-4">{{ run.status?.cost?.gpuHours ?? '—' }}</td>
                <td class="py-3 pr-4 font-mono text-xs text-gray-400">{{ run.status?.reportUri || '—' }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </section>

      <section class="grid gap-4 md:grid-cols-2">
        <div class="bg-gray-900/60 p-4 rounded border border-gray-700">
          <h2 class="text-xl font-semibold mb-3">Benchmark Suites</h2>
          <div v-if="benchmarkSuites.length === 0" class="text-sm text-gray-500">No benchmark suites found.</div>
          <div v-for="suite in benchmarkSuites" :key="suite.metadata.uid" class="py-3 border-b border-gray-800 last:border-b-0">
            <div class="flex justify-between gap-4">
              <span class="text-pink-300 font-medium">{{ suite.metadata.name }}</span>
              <span class="text-xs text-gray-400">{{ suite.spec?.taxonomy || 'unknown' }}</span>
            </div>
            <div class="text-xs text-gray-500 mt-1">
              version {{ suite.spec?.suiteVersion || '—' }} · tasks {{ suite.status?.taskCount ?? suite.spec?.tasks?.length ?? 0 }} · ready {{ suite.status?.ready ?? false }}
            </div>
          </div>
        </div>

        <div class="bg-gray-900/60 p-4 rounded border border-gray-700">
          <h2 class="text-xl font-semibold mb-3">Experiments</h2>
          <div v-if="experiments.length === 0" class="text-sm text-gray-500">No experiments found.</div>
          <div v-for="exp in experiments" :key="exp.metadata.uid" class="py-3 border-b border-gray-800 last:border-b-0">
            <div class="flex justify-between gap-4">
              <span class="text-pink-300 font-medium">{{ exp.metadata.name }}</span>
              <span class="text-xs text-gray-400">{{ exp.status?.phase || 'Unknown' }}</span>
            </div>
            <div v-if="exp.status?.workspacePath" class="text-xs font-mono text-gray-500 mt-1 overflow-x-auto">
              {{ exp.status.workspacePath }}
            </div>
          </div>
        </div>
      </section>
    </main>
  </div>
</template>

<script setup lang="ts">
import { context, trace } from '@opentelemetry/api'
import { ref, onMounted } from 'vue'
import { injectTraceHeaders, startUiSpan } from './telemetry'

const experiments = ref<any[]>([])
const benchmarkSuites = ref<any[]>([])
const benchmarkRuns = ref<any[]>([])
const loading = ref(true)
const error = ref<string | null>(null)

const fetchJson = async (path: string) => {
  const span = startUiSpan(`fetch ${path}`)
  return await context.with(trace.setSpan(context.active(), span), async () => {
    try {
      const headers = injectTraceHeaders()
      const res = await fetch(path, { headers })
      if (!res.ok) {
        const errText = await res.text()
        throw new Error(`Error ${res.status} from ${path}: ${errText}`)
      }
      return await res.json()
    } catch (e) {
      span.recordException(e as Error)
      throw e
    } finally {
      span.end()
    }
  })
}

const refreshAll = async () => {
  const span = startUiSpan('refresh athena resources')
  loading.value = true
  error.value = null
  try {
    const [expData, suiteData, runData] = await Promise.all([
      fetchJson('/api/v1/experiments'),
      fetchJson('/api/v1/benchmark-suites'),
      fetchJson('/api/v1/benchmark-runs'),
    ])
    experiments.value = expData
    benchmarkSuites.value = suiteData
    benchmarkRuns.value = runData
  } catch (e: any) {
    span.recordException(e)
    console.error(e)
    error.value = e.message || 'Failed to fetch Athena resources'
  } finally {
    span.end()
    loading.value = false
  }
}

onMounted(() => {
  refreshAll()
})
</script>
