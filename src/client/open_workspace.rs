use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::api::schema::ClientOpenWorkspaceReason;
use crate::client::endpoint::{ClientEndpointId, EndpointCatalog};

pub(crate) fn open_workspace(
    endpoint_id: &ClientEndpointId,
    catalog: &EndpointCatalog,
    path: &str,
    opener: Option<&str>,
) -> Result<Child, (ClientOpenWorkspaceReason, String)> {
    let argv = open_workspace_argv(endpoint_id, catalog, path, opener)?;
    Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            (
                ClientOpenWorkspaceReason::LaunchFailed,
                format!("failed to launch {}: {error}", argv[0]),
            )
        })
}

fn open_workspace_argv(
    endpoint_id: &ClientEndpointId,
    catalog: &EndpointCatalog,
    path: &str,
    opener: Option<&str>,
) -> Result<Vec<String>, (ClientOpenWorkspaceReason, String)> {
    open_workspace_argv_with_remote_target(
        endpoint_id,
        catalog,
        path,
        opener,
        standalone_remote_target().as_deref(),
    )
}

fn open_workspace_argv_with_remote_target(
    endpoint_id: &ClientEndpointId,
    catalog: &EndpointCatalog,
    path: &str,
    opener: Option<&str>,
    standalone_remote_target: Option<&str>,
) -> Result<Vec<String>, (ClientOpenWorkspaceReason, String)> {
    let remote = matches!(endpoint_id, ClientEndpointId::Ssh(_))
        || (matches!(endpoint_id, ClientEndpointId::Local) && standalone_remote_target.is_some());
    let path_is_absolute = if remote {
        path.starts_with('/')
    } else {
        Path::new(path).is_absolute()
    };
    if !path_is_absolute {
        return Err((
            ClientOpenWorkspaceReason::InvalidPath,
            format!("workspace path is not absolute: {path}"),
        ));
    }

    let opener = opener.unwrap_or("zed");
    if opener != "zed" {
        return Err((
            ClientOpenWorkspaceReason::UnsupportedOpener,
            format!("unsupported workspace opener: {opener}"),
        ));
    }

    let zed = std::env::var("ZED_BIN")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "zed".to_string());

    let open_path = match endpoint_id {
        ClientEndpointId::Local => match standalone_remote_target {
            Some(target) => remote_ssh_uri(target, path),
            None => path.to_string(),
        },
        ClientEndpointId::Ssh(_) => {
            let target = catalog.target_for_endpoint(endpoint_id).ok_or_else(|| {
                (
                    ClientOpenWorkspaceReason::LaunchFailed,
                    "saved SSH target is unavailable for the active endpoint".to_string(),
                )
            })?;
            remote_ssh_uri(target, path)
        }
    };

    Ok(vec![zed, "-n".to_string(), open_path])
}

fn standalone_remote_target() -> Option<String> {
    std::env::var(crate::remote::REMOTE_TARGET_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn remote_ssh_uri(target: &str, path: &str) -> String {
    let target = target.strip_prefix("ssh://").unwrap_or(target);
    format!(
        "ssh://{}{}",
        target.trim_end_matches('/'),
        if path.starts_with('/') {
            path.to_string()
        } else {
            format!("/{path}")
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> (EndpointCatalog, ClientEndpointId) {
        let mut catalog = EndpointCatalog::default();
        let profile_id = catalog
            .add_ssh("Build", "dev@build.example:2222", "agents")
            .unwrap();
        (catalog, ClientEndpointId::Ssh(profile_id))
    }

    #[test]
    fn local_workspace_uses_the_local_path() {
        let argv = open_workspace_argv_with_remote_target(
            &ClientEndpointId::Local,
            &catalog().0,
            "/repo",
            None,
            None,
        )
        .unwrap();
        assert_eq!(argv[1..], ["-n", "/repo"]);
    }

    #[test]
    fn standalone_remote_workspace_uses_the_attach_target() {
        let argv = open_workspace_argv_with_remote_target(
            &ClientEndpointId::Local,
            &catalog().0,
            "/repo",
            None,
            Some("workbox"),
        )
        .unwrap();
        assert_eq!(argv[1..], ["-n", "ssh://workbox/repo"]);
    }

    #[test]
    fn remote_workspace_uses_the_saved_ssh_target() {
        let (catalog, endpoint_id) = catalog();
        let argv =
            open_workspace_argv(&endpoint_id, &catalog, "/home/me/project", Some("zed")).unwrap();
        assert_eq!(
            argv[1..],
            ["-n", "ssh://dev@build.example:2222/home/me/project"]
        );
    }

    #[test]
    fn relative_paths_and_unknown_openers_are_rejected() {
        assert_eq!(
            open_workspace_argv(&ClientEndpointId::Local, &catalog().0, "repo", None)
                .unwrap_err()
                .0,
            ClientOpenWorkspaceReason::InvalidPath
        );
        assert_eq!(
            open_workspace_argv(
                &ClientEndpointId::Local,
                &catalog().0,
                "/repo",
                Some("unknown"),
            )
            .unwrap_err()
            .0,
            ClientOpenWorkspaceReason::UnsupportedOpener
        );
    }
}
