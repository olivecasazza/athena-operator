package main

import (
	"context"
	"log"
	"net/http"
	"os"
	"strings"
	"time"

	"go.opentelemetry.io/contrib/instrumentation/net/http/otelhttp"
	"go.opentelemetry.io/otel"
	"go.opentelemetry.io/otel/attribute"
	"go.opentelemetry.io/otel/exporters/otlp/otlpmetric/otlpmetricgrpc"
	"go.opentelemetry.io/otel/exporters/otlp/otlptrace/otlptracegrpc"
	"go.opentelemetry.io/otel/metric"
	"go.opentelemetry.io/otel/propagation"
	"go.opentelemetry.io/otel/sdk/resource"
	sdkmetric "go.opentelemetry.io/otel/sdk/metric"
	sdktrace "go.opentelemetry.io/otel/sdk/trace"
	semconv "go.opentelemetry.io/otel/semconv/v1.37.0"
	"go.opentelemetry.io/otel/trace"
)

const defaultAPIServiceName = "athena-console-api"

type telemetryShutdown func(context.Context) error

var (
	apiMeter                = otel.Meter(defaultAPIServiceName)
	apiHTTPRequestDuration metric.Float64Histogram
)

func initTelemetry(ctx context.Context) telemetryShutdown {
	otel.SetTextMapPropagator(propagation.TraceContext{})

	res, err := resource.New(ctx,
		resource.WithFromEnv(),
		resource.WithAttributes(
			semconv.ServiceName(serviceName(defaultAPIServiceName)),
			semconv.ServiceVersion("0.1.0"),
		),
	)
	if err != nil {
		log.Printf("failed to initialize OTEL resource: %v", err)
	}

	shutdowns := []telemetryShutdown{}
	if otlpEndpoint() != "" {
		traceExporter, err := otlptracegrpc.New(ctx)
		if err != nil {
			log.Printf("failed to initialize OTLP trace exporter: %v", err)
		} else {
			tp := sdktrace.NewTracerProvider(
				sdktrace.WithResource(res),
				sdktrace.WithSampler(sdktrace.ParentBased(sdktrace.TraceIDRatioBased(sampleRatio()))),
				sdktrace.WithBatcher(traceExporter),
			)
			otel.SetTracerProvider(tp)
			shutdowns = append(shutdowns, tp.Shutdown)
		}

		metricExporter, err := otlpmetricgrpc.New(ctx)
		if err != nil {
			log.Printf("failed to initialize OTLP metric exporter: %v", err)
		} else {
			mp := sdkmetric.NewMeterProvider(
				sdkmetric.WithResource(res),
				sdkmetric.WithReader(sdkmetric.NewPeriodicReader(metricExporter, sdkmetric.WithInterval(15*time.Second))),
			)
			otel.SetMeterProvider(mp)
			apiMeter = otel.Meter(defaultAPIServiceName)
			shutdowns = append(shutdowns, mp.Shutdown)
		}
	}

	apiHTTPRequestDuration, err = apiMeter.Float64Histogram(
		"http.server.request.duration",
		metric.WithDescription("Athena Console API HTTP server request duration in seconds"),
		metric.WithUnit("s"),
	)
	if err != nil {
		log.Printf("failed to initialize HTTP duration metric: %v", err)
	}

	return func(ctx context.Context) error {
		var lastErr error
		for i := len(shutdowns) - 1; i >= 0; i-- {
			if err := shutdowns[i](ctx); err != nil {
				lastErr = err
			}
		}
		return lastErr
	}
}

func otlpEndpoint() string {
	return strings.TrimSpace(os.Getenv("OTEL_EXPORTER_OTLP_ENDPOINT"))
}

func serviceName(defaultName string) string {
	if value := strings.TrimSpace(os.Getenv("OTEL_SERVICE_NAME")); value != "" {
		return value
	}
	return defaultName
}

func sampleRatio() float64 {
	return 1.0
}

func instrumentHandler(name string, handler http.Handler) http.Handler {
	return otelhttp.NewHandler(
		http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			startedAt := time.Now()
			handler.ServeHTTP(w, r)
			if apiHTTPRequestDuration != nil {
				apiHTTPRequestDuration.Record(r.Context(), time.Since(startedAt).Seconds(), metric.WithAttributes(
					attribute.String("http.route", name),
					attribute.String("http.request.method", r.Method),
				))
			}
			span := trace.SpanFromContext(r.Context())
			if span.SpanContext().IsValid() {
				log.Printf("trace_id=%s span_id=%s route=%s method=%s", span.SpanContext().TraceID(), span.SpanContext().SpanID(), name, r.Method)
			}
		}),
		name,
	)
}

func instrumentHandlerFunc(name string, handler http.HandlerFunc) http.Handler {
	return instrumentHandler(name, handler)
}
