use std::path::PathBuf;
use std::time::{Duration, Instant};

use tracing::{debug, warn};

use super::{api, HeadlessServer};
use crate::api::schema::{
    ClientOpenWorkspaceReason, ClientTarget, ResponseResult, SuccessResponse,
};
use crate::protocol::endpoint::{
    EndpointOpenWorkspaceRequest, EndpointOpenWorkspaceResult, CLIENT_OPEN_WORKSPACE_CAPABILITY,
};
use crate::server::clients::ClientConnectionMode;

const CLIENT_OPEN_WORKSPACE_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) struct PendingClientOpenWorkspace {
    client_id: u64,
    api_id: Option<String>,
    respond_to: Option<std::sync::mpsc::Sender<String>>,
    deadline: Instant,
}

impl PendingClientOpenWorkspace {
    fn into_responder(self) -> OpenWorkspaceResponder {
        match (self.api_id, self.respond_to) {
            (Some(id), Some(respond_to)) => OpenWorkspaceResponder::Api { id, respond_to },
            _ => OpenWorkspaceResponder::Keybind,
        }
    }
}

struct PreparedClientOpenWorkspace {
    client_id: u64,
    path: PathBuf,
}

enum OpenWorkspaceResponder {
    /// Answer an API caller with the client's result.
    Api {
        id: String,
        respond_to: std::sync::mpsc::Sender<String>,
    },
    /// A keybinding requested the open; only failures are logged.
    Keybind,
}

impl HeadlessServer {
    pub(super) fn handle_client_open_workspace_api(&mut self, msg: api::ApiRequestMessage) -> bool {
        let api::schema::Method::ClientOpenWorkspace(params) = &msg.request.method else {
            return false;
        };
        let workspace_id = params.workspace_id.clone();
        let opener = params.opener.clone();
        let target = params.target.clone();
        let responder = OpenWorkspaceResponder::Api {
            id: msg.request.id,
            respond_to: msg.respond_to,
        };
        if let Err((reason, message, OpenWorkspaceResponder::Api { id, respond_to })) =
            self.request_client_open_workspace(&workspace_id, opener.as_deref(), &target, responder)
        {
            let _ = respond_to.send(open_workspace_response(id, false, reason, Some(message)));
        }
        false
    }

    /// Dispatches queued `type = "open_workspace"` keybinding requests.
    pub(super) fn drain_client_open_workspace_intents(&mut self) {
        let intents = std::mem::take(&mut self.app.pending_client_open_workspace);
        for intent in intents {
            let target = intent
                .invoking_client_token
                .as_deref()
                .filter(|token| token.starts_with("endpoint:"))
                .map(|token| ClientTarget::Invocation {
                    invocation_id: token.to_string(),
                })
                .unwrap_or(ClientTarget::Foreground);
            if let Err((reason, message, _)) = self.request_client_open_workspace(
                &intent.workspace_id,
                Some(intent.opener.as_str()),
                &target,
                OpenWorkspaceResponder::Keybind,
            ) {
                warn!(
                    ?reason,
                    %message,
                    workspace_id = %intent.workspace_id,
                    "open-workspace keybinding failed"
                );
            }
        }
    }

    fn request_client_open_workspace(
        &mut self,
        workspace_id: &str,
        opener: Option<&str>,
        target: &ClientTarget,
        responder: OpenWorkspaceResponder,
    ) -> Result<(), (ClientOpenWorkspaceReason, String, OpenWorkspaceResponder)> {
        let prepared = match self.prepare_client_open_workspace(workspace_id, target) {
            Ok(prepared) => prepared,
            Err((reason, message)) => return Err((reason, message, responder)),
        };
        self.dispatch_client_open_workspace(prepared, opener.map(str::to_string), responder)
    }

    fn prepare_client_open_workspace(
        &self,
        workspace_id: &str,
        target: &ClientTarget,
    ) -> Result<PreparedClientOpenWorkspace, (ClientOpenWorkspaceReason, String)> {
        let Some(workspace_index) = self.app.parse_workspace_id(workspace_id) else {
            return Err((
                ClientOpenWorkspaceReason::WorkspaceNotFound,
                format!("workspace not found: {workspace_id}"),
            ));
        };
        let Some(path) = self.workspace_open_path(workspace_index) else {
            return Err((
                ClientOpenWorkspaceReason::WorkspaceNotFound,
                format!("workspace has no resolved directory: {workspace_id}"),
            ));
        };
        if !path.is_absolute() {
            return Err((
                ClientOpenWorkspaceReason::InvalidPath,
                format!("workspace path is not absolute: {}", path.display()),
            ));
        }
        let Some(client_id) = self.client_open_workspace_target(target) else {
            return Err((
                ClientOpenWorkspaceReason::NoTargetClient,
                "no client matches the requested open-workspace target".to_string(),
            ));
        };
        let Some(client) = self.clients.get(&client_id) else {
            return Err((
                ClientOpenWorkspaceReason::NoTargetClient,
                format!("client {client_id} is no longer connected"),
            ));
        };
        if !matches!(client.mode, ClientConnectionMode::ClientShell) {
            return Err((
                ClientOpenWorkspaceReason::UnsupportedClient,
                "target client is not a client shell".to_string(),
            ));
        }
        if !client.supports_endpoint_capability(CLIENT_OPEN_WORKSPACE_CAPABILITY) {
            return Err((
                ClientOpenWorkspaceReason::UnsupportedClient,
                "target client does not support client.open_workspace".to_string(),
            ));
        }
        if client.writer.is_none() {
            return Err((
                ClientOpenWorkspaceReason::NoTargetClient,
                "target client connection is closed".to_string(),
            ));
        }
        Ok(PreparedClientOpenWorkspace { client_id, path })
    }

    fn dispatch_client_open_workspace(
        &mut self,
        prepared: PreparedClientOpenWorkspace,
        opener: Option<String>,
        responder: OpenWorkspaceResponder,
    ) -> Result<(), (ClientOpenWorkspaceReason, String, OpenWorkspaceResponder)> {
        let request_id = self.next_client_open_workspace_request_id();
        let endpoint_request = EndpointOpenWorkspaceRequest {
            request_id: request_id.clone(),
            endpoint_boot_id: self.client_shell_boot_id.clone(),
            path: prepared.path.to_string_lossy().into_owned(),
            opener,
        };
        let message =
            match crate::protocol::endpoint::open_workspace_request_message(&endpoint_request) {
                Ok(message) => message,
                Err(error) => {
                    warn!(%error, "failed to encode open-workspace request");
                    return Err((
                        ClientOpenWorkspaceReason::LaunchFailed,
                        format!("failed to encode open-workspace request: {error}"),
                        responder,
                    ));
                }
            };
        let (api_id, respond_to) = match responder {
            OpenWorkspaceResponder::Api { id, respond_to } => (Some(id), Some(respond_to)),
            OpenWorkspaceResponder::Keybind => (None, None),
        };
        self.pending_client_open_workspaces.insert(
            request_id,
            PendingClientOpenWorkspace {
                client_id: prepared.client_id,
                api_id,
                respond_to,
                deadline: Instant::now() + CLIENT_OPEN_WORKSPACE_TIMEOUT,
            },
        );
        if !self.send_to_client(prepared.client_id, message) {
            if let Some(pending) = self
                .pending_client_open_workspaces
                .remove(&endpoint_request.request_id)
            {
                return Err((
                    ClientOpenWorkspaceReason::NoTargetClient,
                    "client connection is closed".to_string(),
                    pending.into_responder(),
                ));
            }
        }
        Ok(())
    }

    pub(super) fn handle_client_open_workspace_result(
        &mut self,
        client_id: u64,
        result: EndpointOpenWorkspaceResult,
    ) -> bool {
        let Some(pending) = self
            .pending_client_open_workspaces
            .remove(&result.request_id)
        else {
            debug!(
                client_id,
                request_id = %result.request_id,
                "ignoring stale open-workspace result"
            );
            return false;
        };
        if pending.client_id != client_id {
            self.pending_client_open_workspaces
                .insert(result.request_id, pending);
            warn!(
                client_id,
                "open-workspace result came from the wrong client"
            );
            return false;
        }
        if !result.opened {
            warn!(
                client_id,
                reason = ?result.reason,
                message = result.message.as_deref().unwrap_or_default(),
                "client failed to open workspace"
            );
        }
        if let (Some(api_id), Some(respond_to)) = (pending.api_id, pending.respond_to) {
            let _ = respond_to.send(open_workspace_response(
                api_id,
                result.opened,
                result.reason,
                result.message,
            ));
        }
        false
    }

    pub(super) fn expire_client_open_workspace_requests(&mut self) {
        let now = Instant::now();
        let expired = self
            .pending_client_open_workspaces
            .iter()
            .filter_map(|(request_id, pending)| {
                (pending.deadline <= now).then_some(request_id.clone())
            })
            .collect::<Vec<_>>();
        for request_id in expired {
            if let Some(pending) = self.pending_client_open_workspaces.remove(&request_id) {
                if let (Some(api_id), Some(respond_to)) = (pending.api_id, pending.respond_to) {
                    let _ = respond_to.send(open_workspace_response(
                        api_id,
                        false,
                        ClientOpenWorkspaceReason::TimedOut,
                        Some("the target client did not answer in time".to_string()),
                    ));
                } else {
                    warn!(request_id = %request_id, "open-workspace keybinding timed out");
                }
            }
        }
    }

    pub(super) fn fail_client_open_workspaces_for_disconnected_client(&mut self, client_id: u64) {
        let request_ids = self
            .pending_client_open_workspaces
            .iter()
            .filter_map(|(request_id, pending)| {
                (pending.client_id == client_id).then_some(request_id.clone())
            })
            .collect::<Vec<_>>();
        for request_id in request_ids {
            if let Some(pending) = self.pending_client_open_workspaces.remove(&request_id) {
                if let (Some(api_id), Some(respond_to)) = (pending.api_id, pending.respond_to) {
                    let _ = respond_to.send(open_workspace_response(
                        api_id,
                        false,
                        ClientOpenWorkspaceReason::NoTargetClient,
                        Some("the target client disconnected before answering".to_string()),
                    ));
                } else {
                    warn!(
                        client_id,
                        request_id = %request_id,
                        "open-workspace keybinding dropped after client disconnect"
                    );
                }
            }
        }
    }

    fn client_open_workspace_target(&self, target: &ClientTarget) -> Option<u64> {
        match target {
            ClientTarget::Foreground => self.foreground_client_id,
            ClientTarget::Invocation { invocation_id } => {
                let mut parts = invocation_id.strip_prefix("endpoint:")?.splitn(3, ':');
                let boot_id = parts.next()?;
                if boot_id != self.client_shell_boot_id {
                    return None;
                }
                let client_id = parts.next()?.parse::<u64>().ok()?;
                self.clients.contains_key(&client_id).then_some(client_id)
            }
        }
    }

    fn workspace_open_path(&self, workspace_index: usize) -> Option<PathBuf> {
        let workspace = self.app.state.workspaces.get(workspace_index)?;
        workspace
            .worktree_space()
            .map(|space| space.checkout_path.clone())
            .or_else(|| {
                workspace.resolved_identity_cwd_from(
                    &self.app.state.terminals,
                    &self.app.terminal_runtimes,
                )
            })
    }

    fn next_client_open_workspace_request_id(&mut self) -> String {
        let id = self.next_client_open_workspace_request_id;
        self.next_client_open_workspace_request_id =
            self.next_client_open_workspace_request_id.saturating_add(1);
        format!("client-open-workspace-{id}")
    }
}

fn open_workspace_response(
    id: String,
    opened: bool,
    reason: ClientOpenWorkspaceReason,
    message: Option<String>,
) -> String {
    serde_json::to_string(&SuccessResponse {
        id,
        result: ResponseResult::ClientOpenWorkspace {
            opened,
            reason,
            message,
        },
    })
    .unwrap_or_else(|_| "{}".to_string())
}
