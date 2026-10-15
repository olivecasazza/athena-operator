//! A recording fake kube-apiserver for reconcile-level tests.
//!
//! The 2026-10-15 storm was a bug in the operator's API *footprint*: how many
//! requests a pass makes, what they LIST, what they PATCH. Pure-function tests
//! cannot see that. These tests instead run the real reconcile code against a
//! loopback HTTP server that serves canned JSON and records every request, then
//! assert on what was (and was not) asked of the apiserver.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use athena_api::experiment::Experiment;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{Query, State},
    http::{Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use kube::Client;
use kube::Resource;
use kube::runtime::reflector::{self, Store};
use kube::runtime::watcher;
use serde::Serialize;
use serde_json::{Value, json};

use crate::Context;

/// One request the fake apiserver received.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    /// The JSON request body; `Null` when there was none.
    pub body: Value,
}

impl Recorded {
    pub fn label_selector(&self) -> Option<&str> {
        self.query.get("labelSelector").map(String::as_str)
    }
}

struct Route {
    method: Method,
    path: String,
    body: Value,
}

#[derive(Clone)]
struct Shared {
    routes: Arc<Vec<Route>>,
    log: Arc<Mutex<Vec<Recorded>>>,
}

/// A running fake apiserver and a real `kube::Client` pointed at it.
pub struct FakeApi {
    pub client: Client,
    log: Arc<Mutex<Vec<Recorded>>>,
}

impl FakeApi {
    /// Serve `routes` (`(method, exact path, JSON body)`, answered `200`); any other
    /// request gets the `404 Status` a real apiserver returns for a missing object.
    pub async fn start(routes: Vec<(&str, String, Value)>) -> FakeApi {
        let log = Arc::new(Mutex::new(Vec::new()));
        let shared = Shared {
            routes: Arc::new(
                routes
                    .into_iter()
                    .map(|(method, path, body)| Route {
                        method: method.parse().expect("http method"),
                        path,
                        body,
                    })
                    .collect(),
            ),
            log: log.clone(),
        };
        let app = Router::new().fallback(serve).with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let config = kube::Config::new(format!("http://{addr}").parse().expect("uri"));
        let client = Client::try_from(config).expect("client for the fake apiserver");
        FakeApi { client, log }
    }

    /// Every request received so far, oldest first.
    pub fn requests(&self) -> Vec<Recorded> {
        self.log.lock().expect("request log").clone()
    }

    /// Requests with this method whose path ends with `suffix`.
    pub fn calls(&self, method: &str, suffix: &str) -> Vec<Recorded> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == method && r.path.ends_with(suffix))
            .collect()
    }
}

async fn serve(
    State(shared): State<Shared>,
    method: Method,
    uri: Uri,
    Query(query): Query<HashMap<String, String>>,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    shared.log.lock().expect("request log").push(Recorded {
        method: method.to_string(),
        path: path.clone(),
        query,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    });
    match shared
        .routes
        .iter()
        .find(|r| r.method == method && r.path == path)
    {
        Some(route) => (StatusCode::OK, Json(route.body.clone())).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "kind": "Status",
                "apiVersion": "v1",
                "metadata": {},
                "status": "Failure",
                "message": format!("{path} not found"),
                "reason": "NotFound",
                "code": 404,
            })),
        )
            .into_response(),
    }
}

/// URL path of `K`'s collection in `ns`, e.g.
/// `/apis/research.nixlab.io/v1alpha1/namespaces/apps/experiments`.
pub fn collection<K: Resource<DynamicType = ()>>(ns: &str) -> String {
    let (group, version, plural) = (K::group(&()), K::version(&()), K::plural(&()));
    if group.is_empty() {
        format!("/api/{version}/namespaces/{ns}/{plural}")
    } else {
        format!("/apis/{group}/{version}/namespaces/{ns}/{plural}")
    }
}

/// `K` as an apiserver returns it: serialised, with `apiVersion` and `kind`.
pub fn object<K: Resource<DynamicType = ()> + Serialize>(item: &K) -> Value {
    let mut value = serde_json::to_value(item).expect("serialise");
    value["apiVersion"] = json!(K::api_version(&()));
    value["kind"] = json!(K::kind(&()));
    value
}

/// A LIST response holding `items`.
pub fn list_of<K: Resource<DynamicType = ()> + Serialize>(items: &[K]) -> Value {
    json!({
        "apiVersion": K::api_version(&()),
        "kind": format!("{}List", K::kind(&())),
        "metadata": { "resourceVersion": "1" },
        "items": items.iter().map(object).collect::<Vec<_>>(),
    })
}

/// The Experiment cache as the Experiment controller's reflector leaves it after
/// its first LIST: primed, holding `items`.
pub fn experiment_cache(items: Vec<Experiment>) -> Store<Experiment> {
    let (store, mut writer) = reflector::store::<Experiment>();
    writer.apply_watcher_event(&watcher::Event::Init);
    for item in items {
        writer.apply_watcher_event(&watcher::Event::InitApply(item));
    }
    writer.apply_watcher_event(&watcher::Event::InitDone);
    store
}

/// A `Context` for `api`'s client with `cache` as its Experiment cache.
pub fn context(api: &FakeApi, cache: Store<Experiment>) -> Arc<Context> {
    Arc::new(Context {
        client: api.client.clone(),
        experiments: cache,
    })
}
