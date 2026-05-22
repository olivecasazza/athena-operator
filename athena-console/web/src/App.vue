<template>
  <div class="min-h-screen bg-gray-950 text-gray-100 flex flex-col">
    <header class="px-8 pt-8 pb-6 text-center shrink-0">
      <h1 class="text-4xl font-bold text-pink-400">Athena Console</h1>
      <p class="text-gray-400 mt-2">Kubernetes Research Operator Dashboard</p>
    </header>

    <main class="w-full max-w-6xl mx-auto bg-gray-800 p-6 rounded-lg shadow-lg space-y-8 flex-1">
      <section>
        <div class="mb-6 flex justify-between items-center gap-4">
          <div>
            <h2 class="text-2xl font-semibold">Benchmark Runs</h2>
            <p class="text-gray-400 text-sm">Early benchmark API scaffold: phase, suite, target, cost, and report links.</p>
          </div>
          <button @click="refreshAll" class="bg-pink-600 hover:bg-pink-500 px-4 py-2 rounded text-sm transition-colors">
            Refresh
          </button>
        </div>

        <div v-if="loading" class="border border-gray-800 bg-black/40 px-4 py-5 font-mono text-xs text-gray-500">
          Loading Athena resources…
        </div>

        <div v-else-if="error" class="border border-red-800 bg-red-950/40 px-4 py-5 font-mono text-xs text-red-200 mb-4 whitespace-pre-wrap break-words">
          {{ error }}
        </div>

        <div v-else-if="benchmarkRuns.length === 0" class="text-gray-500 text-center py-8 px-4 border-2 border-dashed border-gray-700 rounded">
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
            <div class="mt-2 flex flex-wrap items-center gap-2">
              <button class="border border-gray-700 px-3 py-1 font-mono text-xs text-gray-300 hover:border-pink-400 hover:text-white" @click="openIde(exp)">
                Open IDE
              </button>
              <span v-if="draftCounts[draftKey(exp)]" class="font-mono text-xs text-yellow-400">
                {{ draftCounts[draftKey(exp)] }} draft file(s)
              </span>
            </div>
            <div v-if="exp.status?.workspacePath" class="text-xs font-mono text-gray-500 mt-2 overflow-x-auto">
              {{ exp.status.workspacePath }}
            </div>
          </div>
        </div>
      </section>

      <section id="metrics" class="bg-gray-900/60 p-4 rounded border border-gray-700">
        <div class="flex items-center justify-between gap-4">
          <div>
            <h2 class="text-xl font-semibold mb-1">Metrics debugging</h2>
            <p class="text-sm text-gray-500">Use embedded Grafana panels for metric chart debugging instead of custom chart code.</p>
          </div>
          <a class="border border-gray-700 px-3 py-2 font-mono text-xs text-gray-300 hover:border-white hover:text-white" href="/grafana/d/athena-athena-experiment-debugging" target="_blank" rel="noreferrer">
            Open Grafana
          </a>
        </div>
      </section>
    </main>

    <footer class="sticky bottom-0 mt-auto z-40 border-t border-gray-800 bg-black/95 px-4 py-3 backdrop-blur">
      <nav class="mx-auto flex max-w-6xl items-center justify-center gap-2 font-mono text-xs text-gray-500">
        <a class="border border-transparent px-3 py-2 hover:border-white hover:text-white" href="#">overview</a>
        <a class="border border-transparent px-3 py-2 hover:border-white hover:text-white" href="#metrics">metrics</a>
        <button class="border border-transparent px-3 py-2 hover:border-white hover:text-white" @click="experiments[0] && openIde(experiments[0])">ide</button>
        <button class="border border-transparent px-3 py-2 hover:border-white hover:text-white" @click="refreshAll">refresh</button>
      </nav>
    </footer>

    <IdeModal
      :is-open="ideOpen"
      :experiment="selectedExperiment"
      :runtime-profiles="runtimeProfiles"
      :benchmark-suites="benchmarkSuites"
      :benchmark-runs="benchmarkRuns"
      @close="ideOpen = false"
      @save-draft="recordDraftSave"
    />
  </div>
</template>

<script setup lang="ts">
import { context, trace } from '@opentelemetry/api'
import { ref, onMounted } from 'vue'
import IdeModal from './components/IdeModal.vue'
import { injectTraceHeaders, startUiSpan } from './telemetry'

const experiments = ref<any[]>([])
const benchmarkSuites = ref<any[]>([])
const benchmarkRuns = ref<any[]>([])
const runtimeProfiles = ref<any[]>([])
const loading = ref(true)
const error = ref<string | null>(null)
const ideOpen = ref(false)
const selectedExperiment = ref<any | null>(null)
const draftCounts = ref<Record<string, number>>({})

const isJsonContentType = (contentType: string | null) =>
  Boolean(contentType && (contentType.includes('application/json') || contentType.includes('+json')))

const summarizeResponseBody = (body: string, contentType: string | null) => {
  const trimmed = body.trim()
  if (!trimmed) return ''

  if (trimmed.startsWith('<!DOCTYPE html') || trimmed.startsWith('<html') || contentType?.includes('text/html')) {
    const title = trimmed.match(/<title>(.*?)<\/title>/i)?.[1]?.replace(/\s+/g, ' ').trim()
    return title ? `upstream returned HTML instead of JSON (${title})` : 'upstream returned HTML instead of JSON'
  }

  return trimmed.replace(/\s+/g, ' ').slice(0, 240)
}

const fetchJson = async (path: string, fallbackData: any = []) => {
  const span = startUiSpan(`fetch ${path}`)
  return await context.with(trace.setSpan(context.active(), span), async () => {
    try {
      const headers = injectTraceHeaders()
      const res = await fetch(path, { headers })
      const contentType = res.headers.get('content-type')

      if (!res.ok) {
        if (res.status === 404) {
          console.warn(`[fallback] Kubernetes API unavailable from browser path (${path}: 404); rendering cached/mock projection.`)
          return fallbackData
        }

        const errText = await res.text()
        const summary = summarizeResponseBody(errText, contentType)
        throw new Error(
          `Error ${res.status} from ${path}${summary ? `: ${summary}` : ''}`,
        )
      }

      if (!isJsonContentType(contentType)) {
        const responseText = await res.text()
        const summary = summarizeResponseBody(responseText, contentType)
        throw new Error(
          `Invalid response from ${path}: expected JSON but received ${contentType || 'unknown content type'}${summary ? ` (${summary})` : ''}`,
        )
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

const draftKey = (exp: any) => `athena-ide-draft:${exp.metadata?.namespace || 'default'}:${exp.metadata?.name || 'experiment'}`

const refreshDraftCounts = () => {
  const next: Record<string, number> = {}
  for (const exp of experiments.value) {
    const raw = localStorage.getItem(draftKey(exp))
    if (!raw) continue
    try {
      next[draftKey(exp)] = JSON.parse(raw).length
    } catch {
      next[draftKey(exp)] = 1
    }
  }
  draftCounts.value = next
}

const openIde = (exp: any) => {
  selectedExperiment.value = exp
  ideOpen.value = true
}

const recordDraftSave = () => {
  refreshDraftCounts()
}

const refreshAll = async () => {
  const span = startUiSpan('refresh athena resources')
  loading.value = true
  error.value = null
  try {
    const [expData, suiteData, runData, runtimeData] = await Promise.all([
      fetchJson('/api/v1/experiments'),
      fetchJson('/api/v1/benchmark-suites'),
      fetchJson('/api/v1/benchmark-runs'),
      fetchJson('/api/v1/runtime-profiles'),
    ])
    experiments.value = expData
    benchmarkSuites.value = suiteData
    benchmarkRuns.value = runData
    runtimeProfiles.value = runtimeData
    refreshDraftCounts()
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
