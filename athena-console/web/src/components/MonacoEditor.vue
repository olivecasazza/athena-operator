<template>
  <div ref="editorContainer" class="h-full w-full border border-gray-800 bg-black"></div>
</template>

<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref, watch } from 'vue'
import * as monaco from 'monaco-editor'
import 'monaco-editor/esm/vs/basic-languages/yaml/yaml.contribution'

self.MonacoEnvironment = {
  getWorker: async () => {
    const worker = await import('monaco-editor/esm/vs/editor/editor.worker?worker')
    return new worker.default()
  },
}

const props = withDefaults(defineProps<{
  modelValue: string
  language?: string
}>(), {
  language: 'yaml',
})

const emit = defineEmits<{
  'update:modelValue': [value: string]
}>()

const editorContainer = ref<HTMLElement | null>(null)
let editor: monaco.editor.IStandaloneCodeEditor | null = null
let model: monaco.editor.ITextModel | null = null

const themeName = 'athena-dark'

onMounted(() => {
  if (!editorContainer.value) return

  monaco.editor.defineTheme(themeName, {
    base: 'vs-dark',
    inherit: true,
    rules: [
      { token: '', foreground: 'a1a1aa', background: '000000' },
      { token: 'key', foreground: 'f9a8d4' },
      { token: 'string', foreground: 'e4e4e7' },
      { token: 'number', foreground: '93c5fd' },
      { token: 'comment', foreground: '52525b' },
    ],
    colors: {
      'editor.background': '#000000',
      'editor.foreground': '#a1a1aa',
      'editor.lineHighlightBackground': '#18181b',
      'editorLineNumber.foreground': '#3f3f46',
      'editorLineNumber.activeForeground': '#e4e4e7',
      'editor.selectionBackground': '#83184388',
      'editorCursor.foreground': '#f9a8d4',
      'editorIndentGuide.background1': '#27272a',
      'editorIndentGuide.activeBackground1': '#71717a',
    },
  })

  model = monaco.editor.createModel(props.modelValue, props.language)
  editor = monaco.editor.create(editorContainer.value, {
    model,
    theme: themeName,
    automaticLayout: true,
    minimap: { enabled: false },
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace',
    fontSize: 13,
    lineHeight: 20,
    lineNumbersMinChars: 3,
    scrollBeyondLastLine: false,
    wordWrap: 'on',
    tabSize: 2,
    insertSpaces: true,
    padding: { top: 16, bottom: 16 },
    renderLineHighlight: 'line',
  })

  editor.onDidChangeModelContent(() => {
    emit('update:modelValue', editor?.getValue() ?? '')
  })
})

watch(() => props.modelValue, (value) => {
  if (editor && value !== editor.getValue()) {
    editor.setValue(value)
  }
})

watch(() => props.language, (language) => {
  if (model && language) monaco.editor.setModelLanguage(model, language)
})

onBeforeUnmount(() => {
  editor?.dispose()
  model?.dispose()
})
</script>
