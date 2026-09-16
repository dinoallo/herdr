use super::*;

use crate::api::schema::{
    ClientOpenWorkspaceParams, ClientOpenWorkspaceReason, ClientTarget, Method, Request,
    ResponseResult, SuccessResponse,
};
use crate::protocol::endpoint::{
    EndpointOpenWorkspaceRequest, EndpointOpenWorkspaceResult, CLIENT_OPEN_WORKSPACE_CAPABILITY,
    CLIENT_OPEN_WORKSPACE_REQUEST_KIND,
};
use crate::server::clients::{ClientConnection, ClientConnectionMode};

#[test]
fn open_workspace_api_waits_for_the_target_client() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("project")];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let workspace_id = server.app.public_workspace_id(0);

    let (writer, control_rx, _render_rx) = test_client_writer();
    let mut connection = ClientConnection::new_with_mode(
        ClientConnectionMode::ClientShell,
        (80, 24),
        crate::kitty_graphics::HostCellSize::default(),
        1,
        crate::protocol::RenderEncoding::SemanticFrame,
        Some(writer),
    );
    connection.endpoint_capabilities = vec![CLIENT_OPEN_WORKSPACE_CAPABILITY.into()];
    server.clients.insert(7, connection);
    server.foreground_client_id = Some(7);

    let (response_tx, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: Request {
            id: "open-workspace".into(),
            method: Method::ClientOpenWorkspace(ClientOpenWorkspaceParams {
                workspace_id,
                opener: Some("zed".into()),
                target: ClientTarget::Foreground,
            }),
        },
        respond_to: response_tx,
        response_write_complete: None,
        stream_active: None,
    });

    let ServerMessage::EndpointControl { kind, data } =
        read_server_message(control_rx.recv_timeout(Duration::from_secs(1)).unwrap())
    else {
        panic!("expected endpoint control request");
    };
    assert_eq!(kind, CLIENT_OPEN_WORKSPACE_REQUEST_KIND);
    let request: EndpointOpenWorkspaceRequest = serde_json::from_str(&data).unwrap();
    assert_eq!(request.opener.as_deref(), Some("zed"));
    assert!(std::path::Path::new(&request.path).is_absolute());

    assert!(
        !server.handle_server_event(ServerEvent::ClientOpenWorkspaceResult {
            client_id: 7,
            result: EndpointOpenWorkspaceResult {
                request_id: request.request_id,
                opened: true,
                reason: ClientOpenWorkspaceReason::Opened,
                message: None,
            },
        })
    );
    let response: SuccessResponse =
        serde_json::from_str(&response_rx.recv_timeout(Duration::from_secs(1)).unwrap()).unwrap();
    assert_eq!(
        response.result,
        ResponseResult::ClientOpenWorkspace {
            opened: true,
            reason: ClientOpenWorkspaceReason::Opened,
        }
    );

    shutdown_test_runtimes(&mut server);
}

#[test]
fn open_workspace_api_rejects_an_unsupported_client() {
    let mut server = test_headless_server();
    server.app.state.workspaces = vec![crate::workspace::Workspace::test_new("project")];
    server.app.state.active = Some(0);
    server.app.state.ensure_test_terminals();
    let workspace_id = server.app.public_workspace_id(0);

    let (writer, _control_rx, _render_rx) = test_client_writer();
    server.clients.insert(
        9,
        ClientConnection::new(
            (80, 24),
            crate::kitty_graphics::HostCellSize::default(),
            1,
            crate::protocol::RenderEncoding::SemanticFrame,
            Some(writer),
        ),
    );
    server.foreground_client_id = Some(9);

    let (response_tx, response_rx) = std::sync::mpsc::channel();
    server.handle_api_request_with_shutdown_check(crate::api::ApiRequestMessage {
        request: Request {
            id: "open-workspace".into(),
            method: Method::ClientOpenWorkspace(ClientOpenWorkspaceParams {
                workspace_id,
                opener: Some("zed".into()),
                target: ClientTarget::Foreground,
            }),
        },
        respond_to: response_tx,
        response_write_complete: None,
        stream_active: None,
    });

    let response: SuccessResponse =
        serde_json::from_str(&response_rx.recv_timeout(Duration::from_secs(1)).unwrap()).unwrap();
    assert_eq!(
        response.result,
        ResponseResult::ClientOpenWorkspace {
            opened: false,
            reason: ClientOpenWorkspaceReason::UnsupportedClient,
        }
    );

    shutdown_test_runtimes(&mut server);
}
