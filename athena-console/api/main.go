package main

import (
	"context"
	"encoding/json"
	"log"
	"net/http"
	"os"
	"path/filepath"

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
	
	http.HandleFunc("/api/v1/experiments", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if client == nil {
			w.WriteHeader(http.StatusInternalServerError)
			w.Write([]byte(`{"error": "No Kubernetes client"}`))
			return
		}

		gvr := schema.GroupVersionResource{
			Group:    "research.nixlab.io",
			Version:  "v1alpha1",
			Resource: "experiments",
		}

		list, err := client.Resource(gvr).Namespace("").List(context.Background(), metav1.ListOptions{})
		if err != nil {
			log.Printf("Failed to list experiments: %v", err)
			w.WriteHeader(http.StatusInternalServerError)
			w.Write([]byte(`{"error": "Failed to fetch experiments"}`))
			return
		}

		json.NewEncoder(w).Encode(list.Items)
	})

	spa := spaHandler{staticPath: "web/dist", indexPath: "index.html"}
	http.Handle("/", spa)

	port := os.Getenv("PORT")
	if port == "" {
		port = "8080"
	}
	log.Printf("Starting BFF on port %s", port)
	log.Fatal(http.ListenAndServe(":"+port, nil))
}
