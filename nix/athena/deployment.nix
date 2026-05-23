{ lib }:

let
  inherit (lib) filterAttrs;

  deployment = rec {
    chart = {
      name = "athena";
      description = "Athena Kubernetes Research Operator Template";
      version = "0.1.4";
      appVersion = "0.1.4";
    };

    namespace = "athena";
    releaseName = "athena";

    image = {
      repository = "ghcr.io/olivecasazza/athena-operator";
      pullPolicy = "IfNotPresent";
      tag = "latest@sha256:e1f33c4d182c984f8790aba645138f11cc9dc4d92b4fc6c19c2a5aadd88f3211";
    };

    operator = {
      replicas = 1;
      metricsPort = 8080;
      serviceAccountName = "athena-operator";
      resources = {
        requests = {
          cpu = "100m";
          memory = "128Mi";
        };
        limits = {
          memory = "256Mi";
        };
      };
    };

    auth = {
      oidc = {
        enabled = true;
        issuerURL = "https://auth.casazza.io/realms/master";
        usernameClaim = "email";
        groupsClaim = "groups";
        groups = {
          admin = "athena:admin";
          operator = "athena:operator";
          viewer = "athena:viewer";
        };
      };
      rbac = {
        create = true;
        adminClusterRoleName = "athena-admin";
        operatorClusterRoleName = "athena-operator-role";
      };
    };

    observability = {
      metrics = {
        enabled = true;
        serviceMonitor = {
          enabled = true;
          interval = "15s";
        };
      };
      grafanaDashboard.enabled = true;
    };

    api = {
      group = "research.nixlab.io";
      version = "v1alpha1";
      resources = [
        "runtimeprofiles"
        "experimenttemplates"
        "researchcampaigns"
        "experiments"
        "benchmarksuites"
        "benchmarkruns"
        "metricsources"
      ];
      statusResources = map (resource: "${resource}/status") api.resources;
    };
  };

  labels = {
    "app.kubernetes.io/name" = deployment.chart.name;
    "app.kubernetes.io/instance" = deployment.releaseName;
    "app.kubernetes.io/version" = deployment.chart.appVersion;
    "app.kubernetes.io/managed-by" = "Helm";
    "helm.sh/chart" = "${deployment.chart.name}-${deployment.chart.version}";
  };

  selectorLabels = filterAttrs (
    name: _value:
    builtins.elem name [
      "app.kubernetes.io/name"
      "app.kubernetes.io/instance"
    ]
  ) labels;

  fullname = deployment.releaseName;

  operatorRules = [
    {
      apiGroups = [ deployment.api.group ];
      resources = deployment.api.resources;
      verbs = [
        "get"
        "list"
        "watch"
      ];
    }
    {
      apiGroups = [ deployment.api.group ];
      resources = [ "benchmarkruns" ];
      verbs = [
        "create"
        "update"
        "patch"
        "delete"
      ];
    }
    {
      apiGroups = [ deployment.api.group ];
      resources = deployment.api.statusResources;
      verbs = [
        "get"
        "patch"
        "update"
      ];
    }
    {
      apiGroups = [ "batch" ];
      resources = [ "jobs" ];
      verbs = [
        "get"
        "list"
        "watch"
        "create"
        "update"
        "patch"
        "delete"
      ];
    }
    {
      apiGroups = [ "" ];
      resources = [
        "pods"
        "pods/log"
        "persistentvolumeclaims"
        "events"
      ];
      verbs = [
        "get"
        "list"
        "watch"
        "create"
        "update"
        "patch"
        "delete"
      ];
    }
  ];

  adminRules = [
    {
      apiGroups = [ deployment.api.group ];
      resources = deployment.api.resources;
      verbs = [
        "get"
        "list"
        "watch"
        "create"
        "update"
        "patch"
        "delete"
      ];
    }
    {
      apiGroups = [ "batch" ];
      resources = [ "jobs" ];
      verbs = [
        "get"
        "list"
        "watch"
        "create"
        "delete"
      ];
    }
    {
      apiGroups = [ "" ];
      resources = [
        "pods"
        "pods/log"
        "persistentvolumeclaims"
        "events"
      ];
      verbs = [
        "get"
        "list"
        "watch"
      ];
    }
  ];

  dashboardDefaults = {
    schemaVersion = 39;
    tags = [
      "athena"
      "operator"
      "research"
    ];
    timezone = "browser";
    refresh = "30s";
    time = {
      from = "now-6h";
      to = "now";
    };
    templating.list = [
      {
        name = "namespace";
        type = "query";
        datasource.type = "prometheus";
        query = "label_values(athena_operator_reconcile_total, namespace)";
        refresh = 1;
        includeAll = true;
        current.text = "All";
        current.value = "$__all";
      }
      {
        name = "experiment";
        type = "query";
        datasource.type = "prometheus";
        query = ''label_values(athena_operator_agent_action_total{namespace=~"$namespace"}, experiment)'';
        refresh = 1;
        includeAll = true;
        current.text = "All";
        current.value = "$__all";
      }
    ];
  };

  prometheusTarget = expr: legendFormat: {
    inherit expr legendFormat;
    datasource.type = "prometheus";
  };

  statPanel = id: title: expr: gridPos: {
    inherit id title gridPos;
    type = "stat";
    targets = [ (prometheusTarget expr "") ];
    options.reduceOptions.calcs = [ "lastNotNull" ];
  };

  timeseriesPanel = id: title: expr: legendFormat: gridPos: {
    inherit id title gridPos;
    type = "timeseries";
    targets = [ (prometheusTarget expr legendFormat) ];
  };

  tablePanel = id: title: expr: gridPos: {
    inherit id title gridPos;
    type = "table";
    targets = [ (prometheusTarget expr "{{namespace}} {{experiment}} {{action}} {{result}}") ];
  };

  dashboardJson = dashboard: builtins.toJSON (dashboardDefaults // dashboard);

  grafanaDashboards = [
    {
      name = "athena-overview";
      title = "Athena / Overview";
      json = dashboardJson {
        uid = "athena-overview";
        title = "Athena / Overview";
        panels = [
          (statPanel 1 "Running experiments"
            ''sum(athena_experiments_total{phase=~"Running|running",namespace=~"$namespace"})''
            {
              x = 0;
              y = 0;
              w = 6;
              h = 4;
            }
          )
          (statPanel 2 "Succeeded experiments"
            ''sum(athena_experiments_total{phase=~"Succeeded|succeeded",namespace=~"$namespace"})''
            {
              x = 6;
              y = 0;
              w = 6;
              h = 4;
            }
          )
          (statPanel 3 "Failed/Error experiments"
            ''sum(athena_experiments_total{phase=~"Failed|Error|failed|error",namespace=~"$namespace"})''
            {
              x = 12;
              y = 0;
              w = 6;
              h = 4;
            }
          )
          (statPanel 4 "Reconcile error rate"
            ''sum(rate(athena_operator_reconcile_total{namespace=~"$namespace",result="error"}[5m]))''
            {
              x = 18;
              y = 0;
              w = 6;
              h = 4;
            }
          )
          (timeseriesPanel 5 "Experiments by phase"
            ''sum(athena_experiments_total{namespace=~"$namespace"}) by (namespace,campaign,phase)''
            "{{namespace}}/{{campaign}} {{phase}}"
            {
              x = 0;
              y = 4;
              w = 12;
              h = 8;
            }
          )
          (timeseriesPanel 6 "Operator reconcile rate"
            ''sum(rate(athena_operator_reconcile_total{namespace=~"$namespace"}[5m])) by (namespace,campaign,phase,result)''
            "{{namespace}}/{{campaign}} {{phase}} {{result}}"
            {
              x = 12;
              y = 4;
              w = 12;
              h = 8;
            }
          )
          (timeseriesPanel 7 "Agent action rate"
            ''sum(rate(athena_operator_agent_action_total{namespace=~"$namespace"}[5m])) by (namespace,action,result)''
            "{{namespace}} {{action}} {{result}}"
            {
              x = 0;
              y = 12;
              w = 12;
              h = 8;
            }
          )
          (timeseriesPanel 8 "p95 agent action latency"
            ''histogram_quantile(0.95, sum(rate(athena_operator_agent_action_duration_seconds_bucket{namespace=~"$namespace"}[5m])) by (le,namespace,action))''
            "{{namespace}} {{action}}"
            {
              x = 12;
              y = 12;
              w = 12;
              h = 8;
            }
          )
        ];
      };
    }
    {
      name = "athena-operator-telemetry";
      title = "Athena / Operator Telemetry";
      json = dashboardJson {
        uid = "athena-operator-telemetry";
        title = "Athena / Operator Telemetry";
        panels = [
          (timeseriesPanel 1 "Reconcile rate by result"
            ''sum(rate(athena_operator_reconcile_total{namespace=~"$namespace"}[5m])) by (namespace,result)''
            "{{namespace}} {{result}}"
            {
              x = 0;
              y = 0;
              w = 12;
              h = 8;
            }
          )
          (timeseriesPanel 2 "Reconcile p50/p95 latency"
            ''histogram_quantile(0.95, sum(rate(athena_operator_reconcile_duration_seconds_bucket{namespace=~"$namespace"}[5m])) by (le,namespace,campaign))''
            "p95 {{namespace}}/{{campaign}}"
            {
              x = 12;
              y = 0;
              w = 12;
              h = 8;
            }
          )
          (timeseriesPanel 3 "Agent action duration p95"
            ''histogram_quantile(0.95, sum(rate(athena_operator_agent_action_duration_seconds_bucket{namespace=~"$namespace",experiment=~"$experiment"}[5m])) by (le,namespace,experiment,action))''
            "{{namespace}}/{{experiment}} {{action}}"
            {
              x = 0;
              y = 8;
              w = 12;
              h = 8;
            }
          )
          (timeseriesPanel 4 "Agent action errors"
            ''sum(rate(athena_operator_agent_action_total{namespace=~"$namespace",experiment=~"$experiment",result!="ok"}[5m])) by (namespace,experiment,action,result)''
            "{{namespace}}/{{experiment}} {{action}} {{result}}"
            {
              x = 12;
              y = 8;
              w = 12;
              h = 8;
            }
          )
          (tablePanel 5 "Dynamic Athena metric families" ''{__name__=~"athena_.*|athena_operator_.*"}'' {
            x = 0;
            y = 16;
            w = 24;
            h = 8;
          })
        ];
      };
    }
    {
      name = "athena-experiment-debugging";
      title = "Athena / Experiment Debugging";
      json = dashboardJson {
        uid = "athena-experiment-debugging";
        title = "Athena / Experiment Debugging";
        panels = [
          (timeseriesPanel 1 "Selected experiment actions"
            ''sum(rate(athena_operator_agent_action_total{namespace=~"$namespace",experiment=~"$experiment"}[5m])) by (namespace,experiment,action,result)''
            "{{namespace}}/{{experiment}} {{action}} {{result}}"
            {
              x = 0;
              y = 0;
              w = 24;
              h = 8;
            }
          )
          (timeseriesPanel 2 "Selected experiment action p95 latency"
            ''histogram_quantile(0.95, sum(rate(athena_operator_agent_action_duration_seconds_bucket{namespace=~"$namespace",experiment=~"$experiment"}[5m])) by (le,namespace,experiment,action,result))''
            "{{namespace}}/{{experiment}} {{action}} {{result}}"
            {
              x = 0;
              y = 8;
              w = 12;
              h = 8;
            }
          )
          (timeseriesPanel 3 "Selected experiment reconciles"
            ''sum(rate(athena_operator_reconcile_total{namespace=~"$namespace"}[5m])) by (namespace,campaign,phase,result)''
            "{{namespace}}/{{campaign}} {{phase}} {{result}}"
            {
              x = 12;
              y = 8;
              w = 12;
              h = 8;
            }
          )
          (tablePanel 4 "Latest Athena dynamic samples"
            ''{__name__=~"athena_.*|athena_operator_.*",namespace=~"$namespace",experiment=~"$experiment"}''
            {
              x = 0;
              y = 16;
              w = 24;
              h = 8;
            }
          )
        ];
      };
    }
  ];

  renderYaml =
    pkgs: name: value:
    (pkgs.formats.yaml { }).generate name value;

  renderObjects =
    pkgs: name: objects:
    pkgs.runCommand name { nativeBuildInputs = [ pkgs.yq-go ]; } ''
      cp ${renderYaml pkgs "objects.yaml" { items = objects; }} objects.yaml
      yq eval '.items[] | splitDoc' objects.yaml > $out
    '';

  removeNulls =
    value:
    if builtins.isAttrs value then
      filterAttrs (_: v: v != null) (builtins.mapAttrs (_: removeNulls) value)
    else if builtins.isList value then
      map removeNulls value
    else
      value;

  grafanaDashboardsResources = builtins.map (dash: {
    apiVersion = "grafana.integreatly.org/v1beta1";
    kind = "GrafanaDashboard";
    metadata = {
      name = "${fullname}-${dash.name}";
      namespace = "monitoring";
      inherit labels;
    };
    spec = {
      instanceSelector.matchLabels.dashboards = "grafana";
      folder = "Athena";
      json = dash.json;
    };
  }) grafanaDashboards;

  k8sObjects = [
    {
      apiVersion = "v1";
      kind = "ServiceAccount";
      metadata = {
        name = deployment.operator.serviceAccountName;
        inherit labels;
      };
    }
    {
      apiVersion = "apps/v1";
      kind = "Deployment";
      metadata = {
        name = fullname;
        inherit labels;
      };
      spec = {
        replicas = deployment.operator.replicas;
        selector.matchLabels = selectorLabels;
        template = {
          metadata.labels = selectorLabels;
          spec = {
            serviceAccountName = deployment.operator.serviceAccountName;
            imagePullSecrets = helmValues.imagePullSecrets;
            containers = [
              {
                name = deployment.chart.name;
                image = "${deployment.image.repository}:${deployment.image.tag}";
                imagePullPolicy = deployment.image.pullPolicy;
                env = [
                  {
                    name = "METRICS_PORT";
                    value = toString deployment.operator.metricsPort;
                  }
                  {
                    name = "ATHENA_ADMIN_GROUP";
                    value = deployment.auth.oidc.groups.admin;
                  }
                  {
                    name = "ATHENA_OPERATOR_GROUP";
                    value = deployment.auth.oidc.groups.operator;
                  }
                ];
                ports = [
                  {
                    name = "metrics";
                    containerPort = deployment.operator.metricsPort;
                    protocol = "TCP";
                  }
                ];
                resources = deployment.operator.resources;
              }
            ];
          };
        };
      };
    }
    {
      apiVersion = "v1";
      kind = "Service";
      metadata = {
        name = "${fullname}-metrics";
        inherit labels;
      };
      spec = {
        type = "ClusterIP";
        ports = [
          {
            port = deployment.operator.metricsPort;
            targetPort = "metrics";
            protocol = "TCP";
            name = "metrics";
          }
        ];
        selector = selectorLabels;
      };
    }
    {
      apiVersion = "rbac.authorization.k8s.io/v1";
      kind = "ClusterRole";
      metadata = {
        name = deployment.auth.rbac.adminClusterRoleName;
        inherit labels;
      };
      rules = adminRules;
    }
    {
      apiVersion = "rbac.authorization.k8s.io/v1";
      kind = "ClusterRole";
      metadata = {
        name = deployment.auth.rbac.operatorClusterRoleName;
        inherit labels;
      };
      rules = operatorRules;
    }
    {
      apiVersion = "rbac.authorization.k8s.io/v1";
      kind = "ClusterRoleBinding";
      metadata.name = "${fullname}-operator";
      roleRef = {
        apiGroup = "rbac.authorization.k8s.io";
        kind = "ClusterRole";
        name = deployment.auth.rbac.operatorClusterRoleName;
      };
      subjects = [
        {
          kind = "ServiceAccount";
          name = deployment.operator.serviceAccountName;
          namespace = deployment.namespace;
        }
      ];
    }
    {
      apiVersion = "rbac.authorization.k8s.io/v1";
      kind = "ClusterRoleBinding";
      metadata.name = "${fullname}-admin-group-binding";
      roleRef = {
        apiGroup = "rbac.authorization.k8s.io";
        kind = "ClusterRole";
        name = deployment.auth.rbac.adminClusterRoleName;
      };
      subjects = [
        {
          kind = "Group";
          name = deployment.auth.oidc.groups.admin;
          apiGroup = "rbac.authorization.k8s.io";
        }
      ];
    }
    {
      apiVersion = "monitoring.coreos.com/v1";
      kind = "ServiceMonitor";
      metadata = {
        name = fullname;
        inherit labels;
      };
      spec = {
        selector.matchLabels = selectorLabels;
        endpoints = [
          {
            port = "metrics";
            interval = deployment.observability.metrics.serviceMonitor.interval;
          }
        ];
      };
    }
  ]
  ++ grafanaDashboardsResources;

  helmValues = {
    replicaCount = deployment.operator.replicas;
    image = deployment.image // {
      tag = "";
    };
    metrics = {
      enabled = deployment.observability.metrics.enabled;
      port = deployment.operator.metricsPort;
      serviceMonitor = deployment.observability.metrics.serviceMonitor;
    };
    observability.grafanaDashboard.enabled = deployment.observability.grafanaDashboard.enabled;
    serviceAccount = {
      create = true;
      name = deployment.operator.serviceAccountName;
    };
    auth = {
      oidc = deployment.auth.oidc // {
        adminGroup = deployment.auth.oidc.groups.admin;
        operatorGroup = deployment.auth.oidc.groups.operator;
        viewerGroup = deployment.auth.oidc.groups.viewer;
        groups = null;
      };
      rbac = deployment.auth.rbac // {
        adminGroupName = deployment.auth.oidc.groups.admin;
      };
    };
    resources = deployment.operator.resources;
    imagePullSecrets = [ { name = "ghcr-pull"; } ];
  };

  helmTemplates = pkgs: {
    deployment = renderObjects pkgs "deployment.yaml" [
      (builtins.elemAt k8sObjects 1)
    ];
    service = renderObjects pkgs "service.yaml" [
      (builtins.elemAt k8sObjects 2)
    ];
    observability = renderObjects pkgs "observability.yaml" [
      (builtins.elemAt k8sObjects 7)
      (builtins.elemAt k8sObjects 8)
    ];
    rbac = renderObjects pkgs "rbac.yaml" [
      (builtins.elemAt k8sObjects 0)
      (builtins.elemAt k8sObjects 3)
      (builtins.elemAt k8sObjects 4)
      (builtins.elemAt k8sObjects 5)
      (builtins.elemAt k8sObjects 6)
    ];
  };

  helmChart =
    pkgs:
    let
      templates = helmTemplates pkgs;
    in
    pkgs.stdenvNoCC.mkDerivation {
      pname = "athena-helm-chart";
      version = deployment.chart.version;
      src = ../../charts/athena;
      dontBuild = true;
      installPhase = ''
        mkdir -p $out/templates $out/crds
        cp -r crds/. $out/crds/
        cp ${templates.deployment} $out/templates/deployment.yaml
        cp ${templates.service} $out/templates/service.yaml
        cp ${templates.observability} $out/templates/observability.yaml
        cp ${templates.rbac} $out/templates/rbac.yaml
        cp ${renderYaml pkgs "values.yaml" (removeNulls helmValues)} $out/values.yaml
        cp ${
          renderYaml pkgs "Chart.yaml" {
            apiVersion = "v2";
            inherit (deployment.chart)
              name
              description
              version
              appVersion
              ;
            type = "application";
          }
        } $out/Chart.yaml
      '';
    };

  k8sManifests =
    pkgs:
    pkgs.stdenvNoCC.mkDerivation {
      pname = "athena-k8s-manifests";
      version = deployment.chart.version;
      dontUnpack = true;
      nativeBuildInputs = [ pkgs.yq-go ];
      installPhase = ''
        mkdir -p $out
        cp ${renderObjects pkgs "athena-manifests.yaml" k8sObjects} $out/athena-manifests.yaml
        cp ${../../examples/grpo-smoke-template.yaml} grpo-smoke-template.yaml
        cat $out/athena-manifests.yaml grpo-smoke-template.yaml > $out/manifests-merged.yaml
        mv $out/manifests-merged.yaml $out/athena-manifests.yaml
      '';
    };
in
{
  inherit
    deployment
    labels
    selectorLabels
    operatorRules
    adminRules
    helmValues
    k8sObjects
    helmChart
    k8sManifests
    ;
}
