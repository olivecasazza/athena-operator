//! Server-verified access presented to the browser; never inferred from a JWT.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(schemars::JsonSchema))]
pub struct AccessPermissions {
    pub read_research: bool,
    pub write_reports: bool,
    pub read_infrastructure: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "server", derive(schemars::JsonSchema))]
pub struct AccessSession {
    pub profile: String,
    pub namespaces: Vec<String>,
    pub permissions: AccessPermissions,
}
