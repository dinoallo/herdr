//! Client-side opener definitions for `client.open_workspace`.
//!
//! Herdr core does not know any editor. An opener entry names candidate
//! executables plus argv templates, and the client substitutes a small,
//! generic placeholder set when it opens a workspace path.

use serde::{Deserialize, Serialize};

/// Generic placeholders accepted in opener argv templates.
pub(crate) const OPENER_PLACEHOLDERS: &[&str] =
    &["bin", "path", "host", "user", "port", "ssh_target"];

/// One opener definition, resolved by the client that receives the request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default)]
pub struct OpenerConfig {
    /// Identifier used by `herdr open-workspace --opener` and by
    /// `type = "open_workspace"` keybindings.
    pub id: String,
    /// Optional environment variable holding an explicit executable override.
    pub bin_env: Option<String>,
    /// Candidate executables, tried in order. Bare names resolve on `PATH`;
    /// absolute paths are used directly.
    pub bins: Vec<String>,
    /// argv template used for local endpoints.
    pub argv: Vec<String>,
    /// argv template used for remote endpoints.
    pub argv_remote: Option<Vec<String>>,
}

/// Values substituted into opener argv templates.
#[derive(Debug, Clone, Default)]
pub(crate) struct OpenerValues {
    pub(crate) bin: Option<String>,
    pub(crate) tried_bins: Vec<String>,
    pub(crate) path: String,
    pub(crate) host: String,
    pub(crate) user: String,
    pub(crate) port: String,
    pub(crate) ssh_target: String,
}

impl OpenerValues {
    fn placeholder(&self, name: &str) -> Option<&str> {
        match name {
            "bin" => self.bin.as_deref(),
            "path" => Some(self.path.as_str()),
            "host" => Some(self.host.as_str()),
            "user" => Some(self.user.as_str()),
            "port" => Some(self.port.as_str()),
            "ssh_target" => Some(self.ssh_target.as_str()),
            _ => None,
        }
    }

    fn missing_bin_message(&self) -> String {
        if self.tried_bins.is_empty() {
            "no opener executable configured; set bin_env or add a bins entry".to_string()
        } else {
            format!(
                "no opener executable resolved; tried {}",
                self.tried_bins.join(", ")
            )
        }
    }
}

/// Substitutes the supported placeholders in one opener argv template.
pub(crate) fn render_opener_argv(
    template: &[String],
    values: &OpenerValues,
) -> Result<Vec<String>, String> {
    if template.is_empty() {
        return Err("opener argv is empty".to_string());
    }
    let mut rendered = Vec::with_capacity(template.len());
    for argument in template {
        let mut output = String::new();
        let mut rest = argument.as_str();
        while let Some(start) = rest.find('{') {
            output.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            let Some(end) = after.find('}') else {
                output.push_str(&rest[start..]);
                rest = "";
                break;
            };
            let name = &after[..end];
            match values.placeholder(name) {
                Some(value) => output.push_str(value),
                None if name == "bin" => return Err(values.missing_bin_message()),
                None => return Err(format!("unknown placeholder {{{name}}} in opener argv")),
            }
            rest = &after[end + 1..];
        }
        output.push_str(rest);
        rendered.push(output);
    }
    match rendered.first() {
        Some(program) if !program.trim().is_empty() => Ok(rendered),
        _ => Err("opener argv[0] is empty".to_string()),
    }
}

pub(crate) fn opener_diagnostics(openers: &[OpenerConfig]) -> Vec<String> {
    let mut diagnostics = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (index, opener) in openers.iter().enumerate() {
        let field = format!("openers[{index}]");
        let id = opener.id.trim();
        if id.is_empty() {
            diagnostics.push(format!("{field}: missing opener id; ignoring opener"));
            continue;
        }
        if !seen.insert(id.to_string()) {
            diagnostics.push(format!(
                "{field} ({id}): duplicate opener id; the first definition wins"
            ));
        }
        if opener.bins.iter().all(|bin| bin.trim().is_empty())
            && opener
                .bin_env
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
                .is_empty()
        {
            diagnostics.push(format!(
                "{field} ({id}): no bin_env or bins configured; every workspace open will fail"
            ));
        }
        for (name, template) in [
            ("argv", Some(&opener.argv)),
            ("argv_remote", opener.argv_remote.as_ref()),
        ] {
            let Some(template) = template else {
                continue;
            };
            if template.iter().all(|argument| argument.trim().is_empty()) {
                diagnostics.push(format!(
                    "{field} ({id}): empty {name}; this opener cannot use that endpoint kind"
                ));
                continue;
            }
            for (position, argument) in template.iter().enumerate() {
                for placeholder in unknown_placeholders(argument) {
                    diagnostics.push(format!(
                        "{field} ({id}): unknown placeholder {{{placeholder}}} in {name}[{position}]; supported: {}",
                        supported_placeholder_list()
                    ));
                }
            }
        }
    }
    diagnostics
}

fn supported_placeholder_list() -> String {
    OPENER_PLACEHOLDERS
        .iter()
        .map(|name| format!("{{{name}}}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn unknown_placeholders(argument: &str) -> Vec<String> {
    let mut unknown = Vec::new();
    let mut rest = argument;
    while let Some(start) = rest.find('{') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            break;
        };
        let name = &after[..end];
        if !name.is_empty() && !OPENER_PLACEHOLDERS.contains(&name) {
            unknown.push(name.to_string());
        }
        rest = &after[end + 1..];
    }
    unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_parse_from_toml() {
        #[derive(Deserialize)]
        struct Wrapper {
            openers: Vec<OpenerConfig>,
        }

        let wrapper: Wrapper = toml::from_str(
            r#"
[[openers]]
id = "zed"
bin_env = "ZED_BIN"
bins = ["zed", "zedit", "zeditor"]
argv = ["{bin}", "-n", "{path}"]
argv_remote = ["{bin}", "-n", "ssh://{ssh_target}{path}"]
"#,
        )
        .unwrap();
        assert_eq!(wrapper.openers.len(), 1);
        let opener = &wrapper.openers[0];
        assert_eq!(opener.id, "zed");
        assert_eq!(opener.bin_env.as_deref(), Some("ZED_BIN"));
        assert_eq!(opener.bins, ["zed", "zedit", "zeditor"]);
        assert_eq!(opener.argv[1], "-n");
        assert!(opener
            .argv_remote
            .as_ref()
            .is_some_and(|argv| argv[2] == "ssh://{ssh_target}{path}"));
        assert!(opener_diagnostics(&wrapper.openers).is_empty());
    }

    #[test]
    fn renders_supported_placeholders() {
        let values = OpenerValues {
            bin: Some("/usr/bin/editor".to_string()),
            tried_bins: Vec::new(),
            path: "/repo".to_string(),
            host: "build.example".to_string(),
            user: "dev".to_string(),
            port: "2222".to_string(),
            ssh_target: "dev@build.example:2222".to_string(),
        };
        let argv = vec![
            "{bin}".to_string(),
            "-n".to_string(),
            "ssh://{ssh_target}{path}".to_string(),
        ];
        assert_eq!(
            render_opener_argv(&argv, &values).unwrap(),
            vec![
                "/usr/bin/editor".to_string(),
                "-n".to_string(),
                "ssh://dev@build.example:2222/repo".to_string()
            ]
        );
    }

    #[test]
    fn rendering_reports_unresolved_and_unknown_placeholders() {
        let values = OpenerValues {
            tried_bins: vec!["editor".to_string()],
            path: "/repo".to_string(),
            ..OpenerValues::default()
        };
        let unresolved = vec!["{bin}".to_string()];
        assert!(render_opener_argv(&unresolved, &values)
            .unwrap_err()
            .contains("tried editor"));
        let unknown = vec!["--open={pth}".to_string()];
        assert!(render_opener_argv(&unknown, &values)
            .unwrap_err()
            .contains("unknown placeholder"));
    }

    fn opener(id: &str, argv: &[&str]) -> OpenerConfig {
        OpenerConfig {
            id: id.to_string(),
            bins: vec!["example".to_string()],
            argv: argv.iter().map(|arg| arg.to_string()).collect(),
            ..OpenerConfig::default()
        }
    }

    #[test]
    fn unknown_placeholders_are_reported() {
        assert_eq!(
            unknown_placeholders("{bin} -n {path}"),
            Vec::<String>::new()
        );
        assert_eq!(
            unknown_placeholders("--open={pth}"),
            vec!["pth".to_string()]
        );
        assert_eq!(
            unknown_placeholders("ssh://{host}{path}"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn diagnostics_flag_duplicates_and_typos() {
        let openers = vec![
            opener("zed", &["{bin}", "-n", "{pth}"]),
            opener("zed", &["{bin}"]),
        ];
        let diagnostics = opener_diagnostics(&openers);
        assert!(diagnostics
            .iter()
            .any(|line| line.contains("unknown placeholder")));
        assert!(diagnostics
            .iter()
            .any(|line| line.contains("duplicate opener id")));
    }

    #[test]
    fn diagnostics_flag_missing_bin_candidates() {
        let openers = vec![OpenerConfig {
            id: "editor".to_string(),
            argv: vec!["{bin}".to_string(), "{path}".to_string()],
            ..OpenerConfig::default()
        }];
        let diagnostics = opener_diagnostics(&openers);
        assert!(diagnostics
            .iter()
            .any(|line| line.contains("no bin_env or bins configured")));
    }

    #[test]
    fn diagnostics_flag_missing_id() {
        let openers = vec![OpenerConfig {
            argv: vec!["{path}".to_string()],
            ..OpenerConfig::default()
        }];
        assert!(opener_diagnostics(&openers)[0].contains("missing opener id"));
    }
}
