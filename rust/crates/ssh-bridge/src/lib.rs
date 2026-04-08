use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshCommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHostSpec {
    pub alias: String,
    pub hostname: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub identity_file: Option<PathBuf>,
    pub proxy_jump: Option<String>,
    pub local_forwards: Vec<TcpForward>,
    pub remote_forwards: Vec<TcpForward>,
    pub dynamic_forwards: Vec<DynamicForward>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TcpForward {
    pub bind_address: String,
    pub bind_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicForward {
    pub bind_address: String,
    pub bind_port: u16,
}

#[derive(Debug, Clone, Default)]
struct HostBlock {
    hostname: Option<String>,
    user: Option<String>,
    port: Option<u16>,
    identity_file: Option<PathBuf>,
    proxy_jump: Option<String>,
    local_forwards: Vec<TcpForward>,
    remote_forwards: Vec<TcpForward>,
    dynamic_forwards: Vec<DynamicForward>,
}

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("ssh config host alias cannot be empty")]
    EmptyAlias,
    #[error("ssh config line is malformed: {0}")]
    MalformedConfigLine(String),
    #[error(
        "ssh config global directive `{directive}` is unsupported in strict import mode: {line}"
    )]
    UnsupportedGlobalDirective { directive: String, line: String },
    #[error("ssh config directive `{directive}` is unsupported in strict import mode: {line}")]
    UnsupportedDirective { directive: String, line: String },
    #[error("ssh host pattern `{alias}` is unsupported in strict import mode")]
    UnsupportedHostPattern { alias: String },
    #[error("ssh config field `{field}` is invalid: {value}")]
    InvalidField { field: &'static str, value: String },
    #[error("ssh host `{alias}` is missing HostName")]
    MissingHostName { alias: String },
    #[error("failed to run ssh command")]
    Io(#[source] std::io::Error),
    #[error("ssh command failed: {command} (status: {status})")]
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },
}

pub fn parse_ssh_config(input: &str) -> Result<Vec<SshHostSpec>, SshError> {
    let mut hosts = Vec::new();
    let mut aliases: Vec<String> = Vec::new();
    let mut block = HostBlock::default();

    for raw_line in input.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut parts = line.split_whitespace();
        let keyword = parts
            .next()
            .ok_or_else(|| SshError::MalformedConfigLine(raw_line.into()))?;
        let values: Vec<_> = parts.collect();
        if values.is_empty() {
            return Err(SshError::MalformedConfigLine(raw_line.into()));
        }

        if keyword.eq_ignore_ascii_case("host") {
            flush_host_block(&mut hosts, &aliases, &block)?;
            aliases = parse_host_aliases(&values)?;
            block = HostBlock::default();
            continue;
        }

        if aliases.is_empty() {
            return Err(SshError::UnsupportedGlobalDirective {
                directive: keyword.into(),
                line: raw_line.into(),
            });
        }

        match keyword.to_ascii_lowercase().as_str() {
            "hostname" => block.hostname = Some(values.join(" ")),
            "user" => block.user = Some(values.join(" ")),
            "port" => {
                block.port = Some(parse_port("Port", values[0])?);
            }
            "identityfile" => block.identity_file = Some(PathBuf::from(values.join(" "))),
            "proxyjump" => block.proxy_jump = Some(values.join(" ")),
            "localforward" => block
                .local_forwards
                .push(parse_tcp_forward("LocalForward", &values)?),
            "remoteforward" => block
                .remote_forwards
                .push(parse_tcp_forward("RemoteForward", &values)?),
            "dynamicforward" => block
                .dynamic_forwards
                .push(parse_dynamic_forward("DynamicForward", values[0])?),
            _ => {
                return Err(SshError::UnsupportedDirective {
                    directive: keyword.into(),
                    line: raw_line.into(),
                })
            }
        }
    }

    flush_host_block(&mut hosts, &aliases, &block)?;
    Ok(hosts)
}

pub fn build_remote_shell_command(
    host: &SshHostSpec,
    remote_command: Option<&str>,
) -> SshCommandSpec {
    let mut args = resolved_host_args(host);
    args.push(host.destination());

    if let Some(remote_command) = remote_command {
        args.push(remote_command.into());
    }

    SshCommandSpec {
        program: "ssh".into(),
        args,
    }
}

pub fn build_tunnel_command(host: &SshHostSpec) -> SshCommandSpec {
    let mut args = resolved_host_args(host);

    for forward in &host.local_forwards {
        args.push("-L".into());
        args.push(format!(
            "{}:{}:{}:{}",
            forward.bind_address, forward.bind_port, forward.target_host, forward.target_port
        ));
    }

    for forward in &host.remote_forwards {
        args.push("-R".into());
        args.push(format!(
            "{}:{}:{}:{}",
            forward.bind_address, forward.bind_port, forward.target_host, forward.target_port
        ));
    }

    for forward in &host.dynamic_forwards {
        args.push("-D".into());
        args.push(format!("{}:{}", forward.bind_address, forward.bind_port));
    }

    args.push("-N".into());
    args.push(host.destination());

    SshCommandSpec {
        program: "ssh".into(),
        args,
    }
}

pub fn probe_ssh_config(config_text: &str, alias: &str) -> Result<String, SshError> {
    let tmp_path = temp_config_path();
    if let Some(parent) = tmp_path.parent() {
        fs::create_dir_all(parent).map_err(SshError::Io)?;
    }
    fs::write(&tmp_path, config_text).map_err(SshError::Io)?;

    let output = Command::new("ssh")
        .arg("-G")
        .arg("-F")
        .arg(&tmp_path)
        .arg(alias)
        .output()
        .map_err(SshError::Io)?;

    let _ = fs::remove_file(&tmp_path);

    if !output.status.success() {
        return Err(SshError::CommandFailed {
            command: format!("ssh -G -F {} {}", tmp_path.display(), alias),
            status: output
                .status
                .code()
                .map(|code| code.to_string())
                .unwrap_or_else(|| "signal".into()),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

impl SshHostSpec {
    fn destination(&self) -> String {
        match self.user.as_deref() {
            Some(user) => format!("{user}@{}", self.hostname),
            None => self.hostname.clone(),
        }
    }
}

fn resolved_host_args(host: &SshHostSpec) -> Vec<String> {
    let mut args = Vec::new();

    if let Some(port) = host.port {
        args.push("-p".into());
        args.push(port.to_string());
    }

    if let Some(identity_file) = host.identity_file.as_ref() {
        args.push("-i".into());
        args.push(identity_file.display().to_string());
    }

    if let Some(proxy_jump) = host.proxy_jump.as_ref() {
        args.push("-J".into());
        args.push(proxy_jump.clone());
    }

    args
}

fn flush_host_block(
    hosts: &mut Vec<SshHostSpec>,
    aliases: &[String],
    block: &HostBlock,
) -> Result<(), SshError> {
    if aliases.is_empty() {
        return Ok(());
    }

    for alias in aliases {
        let alias = alias.trim();
        if alias.is_empty() {
            return Err(SshError::EmptyAlias);
        }

        let hostname = block
            .hostname
            .clone()
            .ok_or_else(|| SshError::MissingHostName {
                alias: alias.to_owned(),
            })?;

        hosts.push(SshHostSpec {
            alias: alias.to_owned(),
            hostname,
            user: block.user.clone(),
            port: block.port,
            identity_file: block.identity_file.clone(),
            proxy_jump: block.proxy_jump.clone(),
            local_forwards: block.local_forwards.clone(),
            remote_forwards: block.remote_forwards.clone(),
            dynamic_forwards: block.dynamic_forwards.clone(),
        });
    }

    Ok(())
}

fn parse_host_aliases(values: &[&str]) -> Result<Vec<String>, SshError> {
    let mut aliases = Vec::with_capacity(values.len());

    for value in values {
        let alias = value.trim();
        if alias.is_empty() {
            return Err(SshError::EmptyAlias);
        }

        if alias.contains(['*', '?', '!']) {
            return Err(SshError::UnsupportedHostPattern {
                alias: alias.into(),
            });
        }

        aliases.push(alias.into());
    }

    Ok(aliases)
}

fn parse_tcp_forward(field: &'static str, values: &[&str]) -> Result<TcpForward, SshError> {
    if values.len() != 2 {
        return Err(SshError::MalformedConfigLine(format!(
            "{field} {}",
            values.join(" ")
        )));
    }

    let (bind_address, bind_port) = parse_bind(values[0], field)?;
    let (target_host, target_port) = parse_target(values[1], field)?;

    Ok(TcpForward {
        bind_address,
        bind_port,
        target_host,
        target_port,
    })
}

fn parse_dynamic_forward(field: &'static str, value: &str) -> Result<DynamicForward, SshError> {
    let (bind_address, bind_port) = parse_bind(value, field)?;
    Ok(DynamicForward {
        bind_address,
        bind_port,
    })
}

fn parse_bind(value: &str, field: &'static str) -> Result<(String, u16), SshError> {
    if let Some((host, port)) = value.rsplit_once(':') {
        if host.chars().all(|char| char.is_ascii_digit()) {
            return Ok(("127.0.0.1".into(), parse_port(field, value)?));
        }

        return Ok((host.to_owned(), parse_port(field, port)?));
    }

    Ok(("127.0.0.1".into(), parse_port(field, value)?))
}

fn parse_target(value: &str, field: &'static str) -> Result<(String, u16), SshError> {
    let (host, port) = value
        .rsplit_once(':')
        .ok_or_else(|| SshError::InvalidField {
            field,
            value: value.into(),
        })?;

    Ok((host.to_owned(), parse_port(field, port)?))
}

fn parse_port(field: &'static str, value: &str) -> Result<u16, SshError> {
    value.parse().map_err(|_| SshError::InvalidField {
        field,
        value: value.into(),
    })
}

fn temp_config_path() -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time should move forward")
        .as_nanos();

    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join(format!("ssh-config-{stamp}.tmp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openssh_style_config_subset() {
        let config = r#"
Host alpha
  HostName alpha.example
  User dev
  Port 2222
  IdentityFile ~/.ssh/id_ed25519
  ProxyJump bastion
  LocalForward 127.0.0.1:15432 db.internal:5432
  RemoteForward 8080 127.0.0.1:80
  DynamicForward 1080
"#;

        let hosts = parse_ssh_config(config).expect("config should parse");
        assert_eq!(hosts.len(), 1);

        let host = &hosts[0];
        assert_eq!(host.alias, "alpha");
        assert_eq!(host.hostname, "alpha.example");
        assert_eq!(host.user.as_deref(), Some("dev"));
        assert_eq!(host.port, Some(2222));
        assert_eq!(
            host.identity_file.as_ref(),
            Some(&PathBuf::from("~/.ssh/id_ed25519"))
        );
        assert_eq!(host.proxy_jump.as_deref(), Some("bastion"));
        assert_eq!(host.local_forwards.len(), 1);
        assert_eq!(host.remote_forwards.len(), 1);
        assert_eq!(host.dynamic_forwards.len(), 1);
        assert_eq!(host.remote_forwards[0].bind_address, "127.0.0.1");
        assert_eq!(host.remote_forwards[0].bind_port, 8080);
    }

    #[test]
    fn expands_host_aliases() {
        let config = r#"
Host alpha beta
  HostName shared.example
"#;

        let hosts = parse_ssh_config(config).expect("config should parse");
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[0].alias, "alpha");
        assert_eq!(hosts[1].alias, "beta");
        assert_eq!(hosts[0].hostname, "shared.example");
        assert_eq!(hosts[1].hostname, "shared.example");
    }

    #[test]
    fn builds_remote_shell_command() {
        let host = SshHostSpec {
            alias: "alpha".into(),
            hostname: "alpha.example".into(),
            user: Some("dev".into()),
            port: Some(2222),
            identity_file: Some(PathBuf::from("/keys/dev")),
            proxy_jump: Some("bastion".into()),
            local_forwards: Vec::new(),
            remote_forwards: Vec::new(),
            dynamic_forwards: Vec::new(),
        };

        let command = build_remote_shell_command(&host, Some("pwd"));
        assert_eq!(command.program, "ssh");
        assert_eq!(
            command.args,
            vec![
                "-p",
                "2222",
                "-i",
                "/keys/dev",
                "-J",
                "bastion",
                "dev@alpha.example",
                "pwd",
            ]
        );
    }

    #[test]
    fn builds_tunnel_command() {
        let host = SshHostSpec {
            alias: "alpha".into(),
            hostname: "alpha.example".into(),
            user: Some("dev".into()),
            port: None,
            identity_file: None,
            proxy_jump: None,
            local_forwards: vec![TcpForward {
                bind_address: "127.0.0.1".into(),
                bind_port: 15432,
                target_host: "db.internal".into(),
                target_port: 5432,
            }],
            remote_forwards: vec![TcpForward {
                bind_address: "127.0.0.1".into(),
                bind_port: 8080,
                target_host: "127.0.0.1".into(),
                target_port: 80,
            }],
            dynamic_forwards: vec![DynamicForward {
                bind_address: "127.0.0.1".into(),
                bind_port: 1080,
            }],
        };

        let command = build_tunnel_command(&host);
        assert_eq!(command.program, "ssh");
        assert_eq!(
            command.args,
            vec![
                "-L",
                "127.0.0.1:15432:db.internal:5432",
                "-R",
                "127.0.0.1:8080:127.0.0.1:80",
                "-D",
                "127.0.0.1:1080",
                "-N",
                "dev@alpha.example",
            ]
        );
    }

    #[test]
    fn probes_real_openssh_config_parser() {
        let config = r#"
Host alpha
  HostName alpha.example
  User dev
  Port 2222
"#;

        let output = probe_ssh_config(config, "alpha").expect("ssh should parse temp config");
        assert!(output.contains("hostname alpha.example"));
        assert!(output.contains("user dev"));
        assert!(output.contains("port 2222"));
    }

    #[test]
    fn rejects_global_directives_in_strict_import_mode() {
        let error =
            parse_ssh_config("Include ~/.ssh/common.conf\nHost alpha\n  HostName alpha.example\n")
                .expect_err("global directives must be rejected explicitly");

        assert!(matches!(
            error,
            SshError::UnsupportedGlobalDirective { ref directive, .. } if directive == "Include"
        ));
    }

    #[test]
    fn rejects_unsupported_host_directives_in_strict_import_mode() {
        let error = parse_ssh_config("Host alpha\n  HostName alpha.example\n  ForwardAgent yes\n")
            .expect_err("unsupported directives must be rejected explicitly");

        assert!(matches!(
            error,
            SshError::UnsupportedDirective { ref directive, .. } if directive == "ForwardAgent"
        ));
    }

    #[test]
    fn rejects_wildcard_host_patterns_in_strict_import_mode() {
        let error = parse_ssh_config("Host *\n  User dev\n")
            .expect_err("wildcard host patterns are outside the strict subset");

        assert!(matches!(
            error,
            SshError::UnsupportedHostPattern { ref alias } if alias == "*"
        ));
    }
}
