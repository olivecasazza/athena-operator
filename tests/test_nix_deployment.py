import json
import subprocess
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def nix_eval_json(attr: str):
    result = subprocess.run(
        ["nix", "eval", "--builders", "", "--json", f".{attr}"],
        cwd=ROOT,
        text=True,
        check=True,
        stdout=subprocess.PIPE,
    )
    return json.loads(result.stdout)


def test_shared_nix_deployment_data_feeds_helm_values():
    deployment = nix_eval_json("#lib.athena.deployment")
    values = nix_eval_json("#lib.athena.helmValues")

    assert values["metrics"]["port"] == deployment["operator"]["metricsPort"]
    assert values["auth"]["oidc"]["adminGroup"] == deployment["auth"]["oidc"]["groups"]["admin"]
    assert values["auth"]["rbac"]["adminGroupName"] == deployment["auth"]["oidc"]["groups"]["admin"]
    assert values["resources"] == deployment["operator"]["resources"]


def test_shared_nix_deployment_data_feeds_k8s_objects():
    deployment = nix_eval_json("#lib.athena.deployment")
    objects = nix_eval_json("#lib.athena.k8sObjects")

    deployment_obj = next(obj for obj in objects if obj["kind"] == "Deployment")
    container = deployment_obj["spec"]["template"]["spec"]["containers"][0]
    env = {item["name"]: item["value"] for item in container["env"]}

    assert container["image"] == f'{deployment["image"]["repository"]}:{deployment["image"]["tag"]}'
    assert env["METRICS_PORT"] == str(deployment["operator"]["metricsPort"])
    assert env["ATHENA_ADMIN_GROUP"] == deployment["auth"]["oidc"]["groups"]["admin"]
    assert env["ATHENA_OPERATOR_GROUP"] == deployment["auth"]["oidc"]["groups"]["operator"]

    operator_role = next(
        obj
        for obj in objects
        if obj["kind"] == "ClusterRole" and obj["metadata"]["name"] == deployment["auth"]["rbac"]["operatorClusterRoleName"]
    )
    research_rule = operator_role["rules"][0]
    assert research_rule["apiGroups"] == [deployment["api"]["group"]]
    assert research_rule["resources"] == deployment["api"]["resources"]
