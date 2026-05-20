package main

import (
	"context"
	"encoding/json"
	"log"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/client-go/dynamic"
	"k8s.io/client-go/rest"
)

type spaHandler struct {
	staticPath string
	indexPath  string
}

func (h spaHandler) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	path, err := filepath.Abs(r.URL.Path)
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}
	path = filepath.Join(h.staticPath, path)
	_, err = os.Stat(path)
	if os.IsNotExist(err) {
		http.ServeFile(w, r, filepath.Join(h.staticPath, h.indexPath))
		return
	} else if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
	http.FileServer(http.Dir(h.staticPath)).ServeHTTP(w, r)
}

func getKubernetesClient() (dynamic.Interface, error) {
	config, err := rest.InClusterConfig()
	if err != nil {
		return nil, err
	}
	return dynamic.NewForConfig(config)
}

var researchResources = map[string]schema.GroupVersionResource{
	"experiments": {
		Group:    "research.nixlab.io",
		Version:  "v1alpha1",
		Resource: "experiments",
	},
	"benchmark-suites": {
		Group:    "research.nixlab.io",
		Version:  "v1alpha1",
		Resource: "benchmarksuites",
	},
	"benchmark-runs": {
		Group:    "research.nixlab.io",
		Version:  "v1alpha1",
		Resource: "benchmarkruns",
	},
	"metric-sources": {
		Group:    "research.nixlab.io",
		Version:  "v1alpha1",
		Resource: "metricsources",
	},
	"runtime-profiles": {
		Group:    "research.nixlab.io",
		Version:  "v1alpha1",
		Resource: "runtimeprofiles",
	},
	"campaigns": {
		Group:    "research.nixlab.io",
		Version:  "v1alpha1",
		Resource: "researchcampaigns",
	},
}

func listResearchResource(client dynamic.Interface, resourceName string) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if client == nil {
			w.WriteHeader(http.StatusInternalServerError)
			w.Write([]byte(`{"error": "No Kubernetes client"}`))
			return
		}

		gvr, ok := researchResources[resourceName]
		if !ok {
			w.WriteHeader(http.StatusNotFound)
			w.Write([]byte(`{"error": "Unknown resource"}`))
			return
		}

		limit := int64(100)
		if rawLimit := r.URL.Query().Get("limit"); rawLimit != "" {
			parsed, err := strconv.ParseInt(rawLimit, 10, 64)
			if err != nil || parsed < 1 || parsed > 500 {
				w.WriteHeader(http.StatusBadRequest)
				w.Write([]byte(`{"error": "limit must be between 1 and 500"}`))
				return
			}
			limit = parsed
		}

		namespace := r.URL.Query().Get("namespace")
		listOptions := metav1.ListOptions{Limit: limit}
		var (
			items any
			err   error
		)
		if namespace == "" || namespace == "all" {
			unstructuredList, listErr := client.Resource(gvr).Namespace("").List(context.Background(), listOptions)
			items = unstructuredList.Items
			err = listErr
		} else {
			unstructuredList, listErr := client.Resource(gvr).Namespace(namespace).List(context.Background(), listOptions)
			items = unstructuredList.Items
			err = listErr
		}
		if err != nil {
			log.Printf("Failed to list %s: %v", resourceName, err)
			w.WriteHeader(http.StatusInternalServerError)
			w.Write([]byte(`{"error": "Failed to fetch resource"}`))
			return
		}

		json.NewEncoder(w).Encode(items)
	}
}

func main() {
	client, err := getKubernetesClient()
	if err != nil {
		log.Printf("Warning: Could not create Kubernetes client: %v", err)
	}

	http.HandleFunc("/healthz", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		w.Write([]byte("ok\n"))
	})
	http.HandleFunc("/readyz", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		w.Write([]byte("ready\n"))
	})
	http.HandleFunc("/api/v1/me", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		w.Write([]byte(`{"subject": "admin@example.com", "roles": ["athena:admin", "athena:operator", "athena:viewer"]}`))
	})

	for name := range researchResources {
		resourceName := name
		http.HandleFunc("/api/v1/"+resourceName, listResearchResource(client, resourceName))
	}
	http.HandleFunc("/api/experiments", listResearchResource(client, "experiments"))
	http.HandleFunc("/api/benchmark-suites", listResearchResource(client, "benchmark-suites"))
	http.HandleFunc("/api/benchmark-runs", listResearchResource(client, "benchmark-runs"))
	http.HandleFunc("/api/metric-sources", listResearchResource(client, "metric-sources"))
	http.HandleFunc("/api/runtime-profiles", listResearchResource(client, "runtime-profiles"))
	http.HandleFunc("/api/campaigns", listResearchResource(client, "campaigns"))

	spa := spaHandler{staticPath: "web/dist", indexPath: "index.html"}
	http.Handle("/", spa)

	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}
	log.Printf("Starting BFF on port %s", port)
	log.Fatal(http.ListenAndServe(":"+strings.TrimPrefix(port, ":"), nil))
}
