use crate::api::schema::{ClientOpenWorkspaceParams, ClientTarget, Method, Request};

pub(super) fn run_open_workspace_command(args: &[String]) -> std::io::Result<i32> {
    let params = match parse_open_workspace_args(args) {
        Ok(params) => params,
        Err(message) => {
            eprintln!("{message}");
            return Ok(2);
        }
    };

    let response = super::send_request(&Request {
        id: "cli:open-workspace".into(),
        method: Method::ClientOpenWorkspace(params),
    })?;
    if response.get("error").is_some()
        || response
            .pointer("/result/opened")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
    {
        eprintln!("{}", serde_json::to_string(&response).unwrap());
        return Ok(1);
    }
    println!("{}", serde_json::to_string(&response).unwrap());
    Ok(0)
}

fn parse_open_workspace_args(args: &[String]) -> Result<ClientOpenWorkspaceParams, String> {
    let Some(workspace_id) = args.first().cloned() else {
        return Err(usage());
    };
    if matches!(workspace_id.as_str(), "help" | "--help" | "-h") {
        return Err(usage());
    }

    let mut opener = None;
    let mut invocation_id = None;
    let mut foreground = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--opener" => {
                opener = Some(
                    args.get(index + 1)
                        .cloned()
                        .ok_or_else(|| "missing value for --opener".to_string())?,
                );
                index += 2;
            }
            "--invocation-id" => {
                invocation_id = Some(
                    args.get(index + 1)
                        .cloned()
                        .ok_or_else(|| "missing value for --invocation-id".to_string())?,
                );
                index += 2;
            }
            "--foreground" => {
                foreground = true;
                index += 1;
            }
            other => return Err(format!("unknown option: {other}\n{}", usage())),
        }
    }
    if foreground && invocation_id.is_some() {
        return Err("--foreground and --invocation-id cannot be combined".into());
    }

    let target = if foreground {
        ClientTarget::Foreground
    } else if let Some(invocation_id) = invocation_id.or_else(plugin_invocation_token) {
        ClientTarget::Invocation { invocation_id }
    } else {
        ClientTarget::Foreground
    };
    Ok(ClientOpenWorkspaceParams {
        workspace_id,
        opener,
        target,
    })
}

fn plugin_invocation_token() -> Option<String> {
    std::env::var("HERDR_PLUGIN_INVOKING_CLIENT_TOKEN")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn usage() -> String {
    "usage: herdr open-workspace <workspace-id> [--opener zed] [--invocation-id TOKEN|--foreground]"
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_explicit_invocation() {
        let params = parse_open_workspace_args(&[
            "w1".into(),
            "--opener".into(),
            "zed".into(),
            "--invocation-id".into(),
            "invoke-1".into(),
        ])
        .unwrap();
        assert_eq!(params.workspace_id, "w1");
        assert_eq!(params.opener.as_deref(), Some("zed"));
        assert_eq!(
            params.target,
            ClientTarget::Invocation {
                invocation_id: "invoke-1".into()
            }
        );
    }

    #[test]
    fn rejects_conflicting_targets() {
        assert!(parse_open_workspace_args(&[
            "w1".into(),
            "--foreground".into(),
            "--invocation-id".into(),
            "invoke-1".into(),
        ])
        .unwrap_err()
        .contains("cannot be combined"));
    }
}
