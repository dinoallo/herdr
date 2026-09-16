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
    api_id: String,
    respond_to: std::sync::mpsc::Sender<String>,
    deadline: Instant,
}

impl HeadlessServer {
    pub(super) fn handle_client_open_workspace_api(&mut self, msg: api::ApiRequestMessage) -> bool {
        let api::schema::Method::ClientOpenWorkspace(params) = &msg.request.method else {
            return false;
        };

        let Some(workspace_index) = self.app.parse_workspace_id(&params.workspace_id) else {
            let _ = msg.respond_to.send(open_workspace_response(
                msg.request.id,
                false,
                ClientOpenWorkspaceReason::WorkspaceNotFound,
            ));
            return false;
        };
        let Some(path) = self.workspace_open_path(workspace_index) else {
            let _ = msg.respond_to.send(open_workspace_response(
                msg.request.id,
                false,
                ClientOpenWorkspaceReason::WorkspaceNotFound,
            ));
            return false;
        };
        if !path.is_absolute() {
            let _ = msg.respond_to.send(open_workspace_response(
                msg.request.id,
                false,
                ClientOpenWorkspaceReason::InvalidPath,
            ));
            return false;
        }

        let Some(client_id) = self.client_open_workspace_target(&params.target) else {
            let _ = msg.respond_to.send(open_workspace_response(
                msg.request.id,
                false,
                ClientOpenWorkspaceReason::NoTargetClient,
            ));
            return false;
        };
        let Some(client) = self.clients.get(&client_id) else {
            let _ = msg.respond_to.send(open_workspace_response(
                msg.request.id,
                false,
                ClientOpenWorkspaceReason::NoTargetClient,
            ));
            return false;
        };
        if !matches!(client.mode, ClientConnectionMode::ClientShell)
            || !client.supports_endpoint_capability(CLIENT_OPEN_WORKSPACE_CAPABILITY)
        {
            let _ = msg.respond_to.send(open_workspace_response(
                msg.request.id,
                false,
                ClientOpenWorkspaceReason::UnsupportedClient,
            ));
            return false;
        }
        if client.writer.is_none() {
            let _ = msg.respond_to.send(open_workspace_response(
                msg.request.id,
                false,
                ClientOpenWorkspaceReason::NoTargetClient,
            ));
            return false;
        }

        let request_id = self.next_client_open_workspace_request_id();
        let endpoint_request = EndpointOpenWorkspaceRequest {
            request_id: request_id.clone(),
            endpoint_boot_id: self.client_shell_boot_id.clone(),
            path: path.to_string_lossy().into_owned(),
            opener: params.opener.clone(),
        };
        let message =
            match crate::protocol::endpoint::open_workspace_request_message(&endpoint_request) {
                Ok(message) => message,
                Err(error) => {
                    warn!(%error, "failed to encode open-workspace request");
                    let _ = msg.respond_to.send(open_workspace_response(
                        msg.request.id,
                        false,
                        ClientOpenWorkspaceReason::LaunchFailed,
                    ));
                    return false;
                }
            };

        let api_id = msg.request.id;
        self.pending_client_open_workspaces.insert(
            request_id,
            PendingClientOpenWorkspace {
                client_id,
                api_id,
                respond_to: msg.respond_to,
                deadline: Instant::now() + CLIENT_OPEN_WORKSPACE_TIMEOUT,
            },
        );
        if !self.send_to_client(client_id, message) {
            if let Some(pending) = self
                .pending_client_open_workspaces
                .remove(&endpoint_request.request_id)
            {
                let _ = pending.respond_to.send(open_workspace_response(
                    pending.api_id,
                    false,
                    ClientOpenWorkspaceReason::NoTargetClient,
                ));
            }
        }
        false
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
        let _ = pending.respond_to.send(open_workspace_response(
            pending.api_id,
            result.opened,
            result.reason,
        ));
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
                let _ = pending.respond_to.send(open_workspace_response(
                    pending.api_id,
                    false,
                    ClientOpenWorkspaceReason::TimedOut,
                ));
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
                let _ = pending.respond_to.send(open_workspace_response(
                    pending.api_id,
                    false,
                    ClientOpenWorkspaceReason::NoTargetClient,
                ));
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

    fn workspace_open_path(&self, workspace_index: usize) -> Option<std::path::PathBuf> {
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

fn open_workspace_response(id: String, opened: bool, reason: ClientOpenWorkspaceReason) -> String {
    serde_json::to_string(&SuccessResponse {
        id,
        result: ResponseResult::ClientOpenWorkspace { opened, reason },
    })
    .unwrap_or_else(|_| "{}".to_string())
}
