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
	"time"

	authv1 "k8s.io/api/authorization/v1"
	metav1 "k8s.io/apimachinery/pkg/apis/meta/v1"
	"k8s.io/apimachinery/pkg/apis/meta/v1/unstructured"
	"k8s.io/apimachinery/pkg/runtime/schema"
	"k8s.io/client-go/dynamic"
	"k8s.io/client-go/kubernetes"
	"k8s.io/client-go/rest"
)

type spaHandler struct {
	staticPath string
	indexPath  string
}

type principal struct {
	Subject string   `json:"subject"`
	Email   string   `json:"email"`
	Groups  []string `json:"groups"`
	Roles   []string `json:"roles"`
}

type authz struct {
	CanView   bool `json:"canView"`
	CanCreate bool `json:"canCreate"`
	CanAdmin  bool `json:"canAdmin"`
}

type meResponse struct {
	principal
	Authz authz `json:"authz"`
}

func splitHeader(raw string) []string {
	seen := map[string]bool{}
	var out []string
	for _, chunk := range strings.FieldsFunc(raw, func(r rune) bool { return r == ',' || r == ';' || r == '|' }) {
		value := strings.TrimSpace(chunk)
		if value == "" || seen[value] {
			continue
		}
		seen[value] = true
		out = append(out, value)
	}
	return out
}

func principalFromHeaders(r *http.Request) principal {
	email := firstNonEmpty(
		r.Header.Get("X-Auth-Request-Email"),
		r.Header.Get("Cf-Access-Authenticated-User-Email"),
		r.Header.Get("X-Forwarded-Email"),
	)
	subject := firstNonEmpty(r.Header.Get("X-Auth-Request-User"), r.Header.Get("X-Forwarded-User"), email)
	groups := splitHeader(firstNonEmpty(r.Header.Get("X-Auth-Request-Groups"), r.Header.Get("X-Forwarded-Groups"), r.Header.Get("X-Auth-Groups")))
	if email == "" {
		email = "anonymous"
	}
	if subject == "" {
		subject = email
	}
	return principal{Subject: subject, Email: email, Groups: groups, Roles: groups}
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if strings.TrimSpace(value) != "" {
			return strings.TrimSpace(value)
		}
	}
	return ""
}

func hasGroup(p principal, group string) bool {
	for _, candidate := range p.Groups {
		if candidate == group {
			return true
		}
	}
	return false
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

func getKubernetesClients() (dynamic.Interface, *kubernetes.Clientset, error) {
	config, err := rest.InClusterConfig()
	if err != nil {
		return nil, nil, err
	}
	dynClient, err := dynamic.NewForConfig(config)
	if err != nil {
		return nil, nil, err
	}
	k8sClient, err := kubernetes.NewForConfig(config)
	return dynClient, k8sClient, err
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
	"experiment-templates": {
		Group:    "research.nixlab.io",
		Version:  "v1alpha1",
		Resource: "experimenttemplates",
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

func dispatchResearchResource(client dynamic.Interface, k8sClient *kubernetes.Clientset) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		if r.Method != http.MethodPost {
			w.WriteHeader(http.StatusMethodNotAllowed)
			return
		}

		var payload map[string]interface{}
		if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
			w.WriteHeader(http.StatusBadRequest)
			w.Write([]byte(`{"error": "Invalid JSON"}`))
			return
		}

		kind, ok := payload["kind"].(string)
		if !ok || kind != "ResearchCampaign" {
			w.WriteHeader(http.StatusBadRequest)
			w.Write([]byte(`{"error": "Unsupported kind"}`))
			return
		}

		gvr := researchResources["campaigns"]

		unstructuredObj := &unstructured.Unstructured{
			Object: payload,
		}

		namespace := unstructuredObj.GetNamespace()
		if namespace == "" {
			namespace = "apps"
		}

		p := principalFromHeaders(r)
		adminGroup := os.Getenv("ATHENA_ADMIN_GROUP")
		if adminGroup == "" {
			adminGroup = "athena:admin"
		}
		opGroup := os.Getenv("ATHENA_OPERATOR_GROUP")
		if opGroup == "" {
			opGroup = "athena:operator"
		}

		if !hasGroup(p, adminGroup) && !hasGroup(p, opGroup) {
			w.WriteHeader(http.StatusForbidden)
			w.Write([]byte(`{"error": "Forbidden"}`))
			return
		}

		created, err := client.Resource(gvr).Namespace(namespace).Create(context.Background(), unstructuredObj, metav1.CreateOptions{})
		if err != nil {
			log.Printf("Failed to create ResearchCampaign: %v", err)
			w.WriteHeader(http.StatusInternalServerError)
			errorBytes, _ := json.Marshal(map[string]interface{}{
				"error": err.Error(),
			})
			w.Write(errorBytes)
			return
		}

		w.Header().Set("Content-Type", "application/json")
		json.NewEncoder(w).Encode(created.Object)
	}
}

func main() {
	ctx := context.Background()
	shutdownTelemetry := initTelemetry(ctx)
	defer func() {
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		if err := shutdownTelemetry(shutdownCtx); err != nil {
			log.Printf("failed to shutdown telemetry: %v", err)
		}
	}()

	client, k8sClient, err := getKubernetesClients()
	if err != nil {
		log.Printf("Warning: Could not create Kubernetes client: %v", err)
	}

	http.Handle("/healthz", instrumentHandlerFunc("GET /healthz", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		w.Write([]byte("ok\n"))
	}))
	http.Handle("/readyz", instrumentHandlerFunc("GET /readyz", func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusOK)
		w.Write([]byte("ready\n"))
	}))
	http.Handle("/api/v1/me", instrumentHandlerFunc("GET /api/v1/me", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		p := principalFromHeaders(r)

		adminGroup := os.Getenv("ATHENA_ADMIN_GROUP")
		if adminGroup == "" {
			adminGroup = "athena:admin"
		}
		opGroup := os.Getenv("ATHENA_OPERATOR_GROUP")
		if opGroup == "" {
			opGroup = "athena:operator"
		}

		az := authz{
			CanView:   true,
			CanCreate: hasGroup(p, adminGroup) || hasGroup(p, opGroup),
			CanAdmin:  hasGroup(p, adminGroup),
		}

		// SelfSubjectAccessReview to check RBAC fallback
		if k8sClient != nil && !az.CanAdmin {
			sar := &authv1.SelfSubjectAccessReview{
				Spec: authv1.SelfSubjectAccessReviewSpec{
					ResourceAttributes: &authv1.ResourceAttributes{
						Group:    "research.nixlab.io",
						Resource: "experiments",
						Verb:     "create",
					},
				},
			}
			res, err := k8sClient.AuthorizationV1().SelfSubjectAccessReviews().Create(context.Background(), sar, metav1.CreateOptions{})
			if err == nil && res.Status.Allowed {
				az.CanCreate = true
			}
		}

		resp := meResponse{
			principal: p,
			Authz:     az,
		}
		json.NewEncoder(w).Encode(resp)
	}))

	for name := range researchResources {
		resourceName := name
		http.Handle("/api/v1/"+resourceName, instrumentHandlerFunc("GET /api/v1/"+resourceName, listResearchResource(client, resourceName)))
	}

	http.Handle("/api/v1/dispatch", instrumentHandlerFunc("POST /api/v1/dispatch", dispatchResearchResource(client, k8sClient)))

	spa := spaHandler{staticPath: "web/dist", indexPath: "index.html"}
	http.Handle("/", instrumentHandler("GET /", spa))

	port := os.Getenv("PORT")
	if port == "" {
		port = "3000"
	}
	log.Printf("Starting BFF on port %s", port)
	log.Fatal(http.ListenAndServe(":"+strings.TrimPrefix(port, ":"), nil))
}
