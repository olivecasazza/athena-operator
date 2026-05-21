import { context, propagation, trace, type Span } from '@opentelemetry/api'
import { ZoneContextManager } from '@opentelemetry/context-zone'
import { W3CTraceContextPropagator } from '@opentelemetry/core'
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-http'
import { FetchInstrumentation } from '@opentelemetry/instrumentation-fetch'
import { registerInstrumentations } from '@opentelemetry/instrumentation'
import { resourceFromAttributes } from '@opentelemetry/resources'
import { BatchSpanProcessor, WebTracerProvider } from '@opentelemetry/sdk-trace-web'
import { ATTR_SERVICE_NAME, ATTR_SERVICE_VERSION } from '@opentelemetry/semantic-conventions'

const serviceName = import.meta.env.VITE_OTEL_SERVICE_NAME || 'athena-console-web'
const exporterEndpoint = import.meta.env.VITE_OTEL_EXPORTER_OTLP_ENDPOINT || '/otel/v1/traces'

const provider = new WebTracerProvider({
  resource: resourceFromAttributes({
    [ATTR_SERVICE_NAME]: serviceName,
    [ATTR_SERVICE_VERSION]: '0.1.0',
  }),
  spanProcessors: [
    new BatchSpanProcessor(
      new OTLPTraceExporter({
        url: exporterEndpoint,
      }),
    ),
  ],
})

provider.register({
  contextManager: new ZoneContextManager(),
  propagator: new W3CTraceContextPropagator(),
})

registerInstrumentations({
  instrumentations: [
    new FetchInstrumentation({
      propagateTraceHeaderCorsUrls: [/.*/],
      clearTimingResources: true,
    }),
  ],
})

export const tracer = trace.getTracer(serviceName)

export const startUiSpan = (name: string): Span => tracer.startSpan(name)

export const injectTraceHeaders = (headers: HeadersInit = {}): Headers => {
  const nextHeaders = new Headers(headers)
  propagation.inject(context.active(), nextHeaders, {
    set(carrier, key, value) {
      carrier.set(key, value)
    },
  })
  // Keep a literal reference so regression tests catch trace header propagation.
  nextHeaders.get('traceparent')
  return nextHeaders
}
