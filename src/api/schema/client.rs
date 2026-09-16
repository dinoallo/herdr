use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ClientOpenWorkspaceParams {
    pub workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opener: Option<String>,
    #[serde(default)]
    pub target: ClientTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClientTarget {
    #[default]
    Foreground,
    Invocation {
        invocation_id: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClientOpenWorkspaceReason {
    Opened,
    NoTargetClient,
    UnsupportedClient,
    EndpointMismatch,
    WorkspaceNotFound,
    InvalidPath,
    UnsupportedOpener,
    LaunchFailed,
    TimedOut,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_workspace_request_round_trips() {
        let request = ClientOpenWorkspaceParams {
            workspace_id: "w1".into(),
            opener: Some("zed".into()),
            target: ClientTarget::Invocation {
                invocation_id: "invoke-1".into(),
            },
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "workspace_id": "w1",
                "opener": "zed",
                "target": {"kind": "invocation", "invocation_id": "invoke-1"}
            })
        );
        assert_eq!(
            serde_json::from_value::<ClientOpenWorkspaceParams>(value).unwrap(),
            request
        );
    }

    #[test]
    fn target_defaults_to_foreground() {
        let request: ClientOpenWorkspaceParams =
            serde_json::from_value(serde_json::json!({"workspace_id": "w1"})).unwrap();
        assert_eq!(request.target, ClientTarget::Foreground);
    }
}
