use k8s_openapi::api::core::v1::{
    Capabilities, PodSecurityContext, SeccompProfile, SecurityContext,
};

pub(crate) fn pod_context(enabled: bool) -> Option<PodSecurityContext> {
    enabled.then(|| PodSecurityContext {
        run_as_non_root: Some(true),
        run_as_user: Some(65532),
        run_as_group: Some(65532),
        fs_group: Some(65532),
        seccomp_profile: Some(SeccompProfile {
            type_: "RuntimeDefault".to_string(),
            ..Default::default()
        }),
        ..Default::default()
    })
}

pub(crate) fn container_context(enabled: bool) -> Option<SecurityContext> {
    enabled.then(|| SecurityContext {
        privileged: Some(false),
        allow_privilege_escalation: Some(false),
        read_only_root_filesystem: Some(true),
        capabilities: Some(Capabilities {
            drop: Some(vec!["ALL".to_string()]),
            ..Default::default()
        }),
        ..Default::default()
    })
}
