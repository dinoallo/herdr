use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::api::schema::ClientOpenWorkspaceReason;
use crate::client::endpoint::{ClientEndpointId, EndpointCatalog};
use crate::config::{render_opener_argv, OpenerConfig, OpenerValues};

pub(crate) fn open_workspace(
    endpoint_id: &ClientEndpointId,
    catalog: &EndpointCatalog,
    openers: &[OpenerConfig],
    path: &str,
    opener: Option<&str>,
) -> Result<Child, (ClientOpenWorkspaceReason, String)> {
    let argv = open_workspace_argv(
        endpoint_id,
        catalog,
        openers,
        path,
        opener,
        standalone_remote_target().as_deref(),
    )?;
    let Some((program, args)) = argv.split_first() else {
        return Err((
            ClientOpenWorkspaceReason::OpenerMisconfigured,
            "opener argv is empty".to_string(),
        ));
    };
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            (
                ClientOpenWorkspaceReason::LaunchFailed,
                format!("failed to launch {program}: {error}"),
            )
        })
}

fn open_workspace_argv(
    endpoint_id: &ClientEndpointId,
    catalog: &EndpointCatalog,
    openers: &[OpenerConfig],
    path: &str,
    opener: Option<&str>,
    standalone_remote_target: Option<&str>,
) -> Result<Vec<String>, (ClientOpenWorkspaceReason, String)> {
    let remote_target = match endpoint_id {
        ClientEndpointId::Ssh(_) => Some(
            catalog
                .target_for_endpoint(endpoint_id)
                .ok_or_else(|| {
                    (
                        ClientOpenWorkspaceReason::LaunchFailed,
                        "saved SSH target is unavailable for the active endpoint".to_string(),
                    )
                })?
                .to_string(),
        ),
        ClientEndpointId::Local => standalone_remote_target.map(str::to_string),
    };
    let remote = remote_target.is_some();
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

    let opener = select_opener(openers, opener)?;
    let template = match (remote, opener.argv_remote.as_deref()) {
        (true, Some(template)) => template,
        (true, None) => {
            return Err((
                ClientOpenWorkspaceReason::OpenerMisconfigured,
                format!(
                    "opener {:?} has no argv_remote; it cannot open remote workspaces",
                    opener.id
                ),
            ))
        }
        (false, _) => opener.argv.as_slice(),
    };

    let (bin, tried_bins) = resolve_opener_bin(opener);
    let (host, user, port, ssh_target) = match remote_target.as_deref() {
        Some(target) => {
            let parts = parse_ssh_target(target);
            (
                parts.host.to_string(),
                parts.user.unwrap_or_default().to_string(),
                parts.port.unwrap_or_default().to_string(),
                target.to_string(),
            )
        }
        None => (String::new(), String::new(), String::new(), String::new()),
    };
    let values = OpenerValues {
        bin,
        tried_bins,
        path: path.to_string(),
        host,
        user,
        port,
        ssh_target,
    };
    render_opener_argv(template, &values)
        .map_err(|message| (ClientOpenWorkspaceReason::OpenerMisconfigured, message))
}

fn select_opener<'a>(
    openers: &'a [OpenerConfig],
    requested: Option<&str>,
) -> Result<&'a OpenerConfig, (ClientOpenWorkspaceReason, String)> {
    let requested = requested.map(str::trim).filter(|id| !id.is_empty());
    match requested {
        Some(id) => openers
            .iter()
            .find(|opener| opener.id.trim() == id)
            .ok_or_else(|| {
                (
                    ClientOpenWorkspaceReason::UnsupportedOpener,
                    format!(
                        "unknown opener {id:?}; available openers: {}",
                        available_openers(openers)
                    ),
                )
            }),
        None => match openers {
            [opener] => Ok(opener),
            [] => Err((
                ClientOpenWorkspaceReason::UnsupportedOpener,
                "no openers are configured; add [[openers]] to config.toml".to_string(),
            )),
            _ => Err((
                ClientOpenWorkspaceReason::UnsupportedOpener,
                format!(
                    "no opener requested and more than one is configured; available openers: {}",
                    available_openers(openers)
                ),
            )),
        },
    }
}

fn available_openers(openers: &[OpenerConfig]) -> String {
    let ids = openers
        .iter()
        .map(|opener| opener.id.trim())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        "(none)".to_string()
    } else {
        ids.join(", ")
    }
}

fn resolve_opener_bin(opener: &OpenerConfig) -> (Option<String>, Vec<String>) {
    let mut tried = Vec::new();
    if let Some(env_name) = opener
        .bin_env
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        tried.push(format!("${env_name}"));
        if let Ok(value) = std::env::var(env_name) {
            let value = value.trim();
            if !value.is_empty() {
                return (Some(value.to_string()), tried);
            }
        }
    }
    for candidate in opener
        .bins
        .iter()
        .map(|bin| bin.trim())
        .filter(|bin| !bin.is_empty())
    {
        tried.push(candidate.to_string());
        if let Some(resolved) = resolve_executable(candidate) {
            return (Some(resolved), tried);
        }
    }
    (None, tried)
}

fn resolve_executable(candidate: &str) -> Option<String> {
    if candidate.contains(std::path::MAIN_SEPARATOR) || candidate.contains('/') {
        return is_executable_file(Path::new(candidate)).then(|| candidate.to_string());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let direct = dir.join(candidate);
        if is_executable_file(&direct) {
            return Some(direct.display().to_string());
        }
        #[cfg(windows)]
        {
            let with_extension = dir.join(format!("{candidate}.exe"));
            if is_executable_file(&with_extension) {
                return Some(with_extension.display().to_string());
            }
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

struct SshTargetParts<'a> {
    user: Option<&'a str>,
    host: &'a str,
    port: Option<&'a str>,
}

fn parse_ssh_target(target: &str) -> SshTargetParts<'_> {
    let authority = target.strip_prefix("ssh://").unwrap_or(target);
    let authority = authority.split('/').next().unwrap_or(authority);
    let (user, host_port) = match authority.rsplit_once('@') {
        Some((user, rest)) => (Some(user).filter(|user| !user.is_empty()), rest),
        None => (None, authority),
    };
    let (host, port) = if let Some(rest) = host_port.strip_prefix('[') {
        match rest.split_once(']') {
            Some((host, tail)) => (host, tail.strip_prefix(':').filter(|port| !port.is_empty())),
            None => (host_port, None),
        }
    } else if host_port.matches(':').count() > 1 {
        // A bare IPv6 literal has no room for a port suffix; keep it whole.
        (host_port, None)
    } else {
        match host_port.rsplit_once(':') {
            Some((host, port))
                if !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()) =>
            {
                (host, Some(port))
            }
            _ => (host_port, None),
        }
    };
    SshTargetParts { user, host, port }
}

fn standalone_remote_target() -> Option<String> {
    std::env::var(crate::remote::REMOTE_TARGET_ENV_VAR)
        .ok()
        .filter(|value| !value.trim().is_empty())
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

    fn literal_opener(id: &str, argv: &[&str]) -> OpenerConfig {
        OpenerConfig {
            id: id.to_string(),
            argv: argv.iter().map(|arg| arg.to_string()).collect(),
            ..OpenerConfig::default()
        }
    }

    #[test]
    fn local_workspace_uses_the_configured_template() {
        let opener = literal_opener("editor", &["editor", "-n", "{path}"]);
        let argv = open_workspace_argv(
            &ClientEndpointId::Local,
            &catalog().0,
            &[opener],
            "/repo",
            Some("editor"),
            None,
        )
        .unwrap();
        assert_eq!(argv, ["editor", "-n", "/repo"]);
    }

    #[test]
    fn remote_workspace_uses_the_saved_ssh_target() {
        let opener = literal_opener(
            "editor",
            &["editor", "-n", "ssh://{user}@{host}:{port}{path}"],
        );
        let mut remote_opener = opener.clone();
        remote_opener.argv_remote = Some(opener.argv.clone());
        let (catalog, endpoint_id) = catalog();
        let argv = open_workspace_argv(
            &endpoint_id,
            &catalog,
            &[remote_opener],
            "/home/me/project",
            Some("editor"),
            None,
        )
        .unwrap();
        assert_eq!(
            argv,
            [
                "editor",
                "-n",
                "ssh://dev@build.example:2222/home/me/project"
            ]
        );
    }

    #[test]
    fn standalone_remote_workspace_uses_the_attach_target() {
        let mut opener = literal_opener("editor", &["editor", "-n", "{path}"]);
        opener.argv_remote = Some(vec![
            "editor".to_string(),
            "-n".to_string(),
            "ssh://{ssh_target}{path}".to_string(),
        ]);
        let argv = open_workspace_argv(
            &ClientEndpointId::Local,
            &catalog().0,
            &[opener],
            "/repo",
            Some("editor"),
            Some("workbox"),
        )
        .unwrap();
        assert_eq!(argv, ["editor", "-n", "ssh://workbox/repo"]);
    }

    #[test]
    fn remote_openers_need_a_remote_template() {
        let opener = literal_opener("editor", &["editor", "{path}"]);
        let (catalog, endpoint_id) = catalog();
        let error = open_workspace_argv(
            &endpoint_id,
            &catalog,
            &[opener],
            "/repo",
            Some("editor"),
            None,
        )
        .unwrap_err();
        assert_eq!(error.0, ClientOpenWorkspaceReason::OpenerMisconfigured);
        assert!(error.1.contains("argv_remote"));
    }

    #[test]
    fn unknown_openers_list_the_available_ids() {
        let opener = literal_opener("editor", &["editor", "{path}"]);
        let error = open_workspace_argv(
            &ClientEndpointId::Local,
            &catalog().0,
            &[opener],
            "/repo",
            Some("missing"),
            None,
        )
        .unwrap_err();
        assert_eq!(error.0, ClientOpenWorkspaceReason::UnsupportedOpener);
        assert!(error.1.contains("available openers: editor"));
    }

    #[test]
    fn a_single_opener_is_used_when_none_is_requested() {
        let opener = literal_opener("editor", &["editor", "{path}"]);
        let argv = open_workspace_argv(
            &ClientEndpointId::Local,
            &catalog().0,
            &[opener],
            "/repo",
            None,
            None,
        )
        .unwrap();
        assert_eq!(argv, ["editor", "/repo"]);
    }

    #[test]
    fn relative_paths_are_rejected() {
        let opener = literal_opener("editor", &["editor", "{path}"]);
        let error = open_workspace_argv(
            &ClientEndpointId::Local,
            &catalog().0,
            &[opener],
            "repo",
            Some("editor"),
            None,
        )
        .unwrap_err();
        assert_eq!(error.0, ClientOpenWorkspaceReason::InvalidPath);
    }

    #[test]
    fn bin_candidates_are_probed_in_order() {
        let dir = std::env::temp_dir().join(format!(
            "herdr-opener-test-{}-{}",
            std::process::id(),
            "probe"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let executable = dir.join("editor");
        std::fs::write(&executable, b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&executable, permissions).unwrap();
        }
        let opener = OpenerConfig {
            id: "editor".to_string(),
            bins: vec![
                "herdr-missing-editor".to_string(),
                executable.display().to_string(),
            ],
            argv: vec!["{bin}".to_string(), "{path}".to_string()],
            ..OpenerConfig::default()
        };
        let argv = open_workspace_argv(
            &ClientEndpointId::Local,
            &catalog().0,
            &[opener],
            "/repo",
            Some("editor"),
            None,
        )
        .unwrap();
        assert_eq!(
            argv,
            [executable.display().to_string(), "/repo".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unresolved_bin_candidates_are_reported() {
        let opener = OpenerConfig {
            id: "editor".to_string(),
            bins: vec!["herdr-missing-editor".to_string()],
            argv: vec!["{bin}".to_string(), "{path}".to_string()],
            ..OpenerConfig::default()
        };
        let error = open_workspace_argv(
            &ClientEndpointId::Local,
            &catalog().0,
            &[opener],
            "/repo",
            Some("editor"),
            None,
        )
        .unwrap_err();
        assert_eq!(error.0, ClientOpenWorkspaceReason::OpenerMisconfigured);
        assert!(error.1.contains("herdr-missing-editor"));
    }

    #[test]
    fn ssh_target_parts_are_parsed() {
        let parts = parse_ssh_target("dev@build.example:2222");
        assert_eq!(parts.user, Some("dev"));
        assert_eq!(parts.host, "build.example");
        assert_eq!(parts.port, Some("2222"));

        let parts = parse_ssh_target("workbox");
        assert_eq!(parts.user, None);
        assert_eq!(parts.host, "workbox");
        assert_eq!(parts.port, None);

        let parts = parse_ssh_target("ssh://dev@[::1]:2200");
        assert_eq!(parts.user, Some("dev"));
        assert_eq!(parts.host, "::1");
        assert_eq!(parts.port, Some("2200"));

        let parts = parse_ssh_target("::1");
        assert_eq!(parts.host, "::1");
        assert_eq!(parts.port, None);
    }
}
