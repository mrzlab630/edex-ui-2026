use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSessionName(String);

impl TmuxSessionName {
    pub fn new(value: impl Into<String>) -> Result<Self, TmuxError> {
        let value = value.into().trim().to_owned();

        if value.is_empty() {
            return Err(TmuxError::EmptySessionName);
        }

        if value.contains(['\n', '\r', '\t', ':']) {
            return Err(TmuxError::InvalidSessionName(value));
        }

        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxCommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxRuntime {
    socket_path: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxSessionSummary {
    pub name: TmuxSessionName,
    pub windows: u32,
    pub attached_clients: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    #[error("tmux session name cannot be empty")]
    EmptySessionName,
    #[error("tmux session name contains unsupported characters: {0}")]
    InvalidSessionName(String),
    #[error("tmux list-sessions line is malformed: {0}")]
    MalformedListLine(String),
    #[error("tmux field `{field}` is not a valid integer: {value}")]
    InvalidInteger { field: &'static str, value: String },
    #[error("failed to execute tmux command")]
    Io(#[source] std::io::Error),
    #[error("tmux command failed: {command} (status: {status})")]
    CommandFailed {
        command: String,
        status: String,
        stderr: String,
    },
}

impl TmuxRuntime {
    pub fn system_default() -> Self {
        Self { socket_path: None }
    }

    pub fn isolated(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: Some(socket_path.into()),
        }
    }

    pub fn create_session(
        &self,
        session_name: &TmuxSessionName,
        cwd: impl AsRef<Path>,
    ) -> Result<(), TmuxError> {
        self.run(build_new_session_command(session_name, cwd))?;
        Ok(())
    }

    pub fn list_sessions(&self) -> Result<Vec<TmuxSessionSummary>, TmuxError> {
        match self.run(build_list_sessions_command()) {
            Ok(output) => parse_list_sessions(&output),
            Err(TmuxError::CommandFailed { stderr, .. }) if is_no_server_running(&stderr) => {
                Ok(Vec::new())
            }
            Err(error) => Err(error),
        }
    }

    pub fn kill_session(&self, session_name: &TmuxSessionName) -> Result<(), TmuxError> {
        self.run(build_kill_session_command(session_name))?;
        Ok(())
    }

    pub fn kill_server(&self) -> Result<(), TmuxError> {
        match self.run(build_kill_server_command()) {
            Ok(_) => Ok(()),
            Err(TmuxError::CommandFailed { stderr, .. }) if is_no_server_running(&stderr) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn run(&self, spec: TmuxCommandSpec) -> Result<String, TmuxError> {
        let mut command = Command::new(&spec.program);

        if let Some(socket_path) = self.socket_path.as_ref() {
            command.arg("-S").arg(socket_path);
        }

        command.args(&spec.args);

        if let Some(cwd) = spec.cwd.as_ref() {
            command.current_dir(cwd);
        }

        let rendered = render_command(&spec, self.socket_path.as_deref());
        let output = command.output().map_err(TmuxError::Io)?;

        if !output.status.success() {
            return Err(TmuxError::CommandFailed {
                command: rendered,
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
}

pub fn build_new_session_command(
    session_name: &TmuxSessionName,
    cwd: impl AsRef<Path>,
) -> TmuxCommandSpec {
    TmuxCommandSpec {
        program: "tmux".into(),
        args: vec![
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            session_name.as_str().into(),
            "-c".into(),
            cwd.as_ref().display().to_string(),
        ],
        cwd: None,
    }
}

pub fn build_attach_session_command(session_name: &TmuxSessionName) -> TmuxCommandSpec {
    TmuxCommandSpec {
        program: "tmux".into(),
        args: vec![
            "attach-session".into(),
            "-t".into(),
            session_name.as_str().into(),
        ],
        cwd: None,
    }
}

pub fn build_list_sessions_command() -> TmuxCommandSpec {
    TmuxCommandSpec {
        program: "tmux".into(),
        args: vec![
            "list-sessions".into(),
            "-F".into(),
            "#{session_name}\t#{session_windows}\t#{session_attached}".into(),
        ],
        cwd: None,
    }
}

pub fn build_kill_server_command() -> TmuxCommandSpec {
    TmuxCommandSpec {
        program: "tmux".into(),
        args: vec!["kill-server".into()],
        cwd: None,
    }
}

pub fn build_kill_session_command(session_name: &TmuxSessionName) -> TmuxCommandSpec {
    TmuxCommandSpec {
        program: "tmux".into(),
        args: vec![
            "kill-session".into(),
            "-t".into(),
            session_name.as_str().into(),
        ],
        cwd: None,
    }
}

pub fn parse_list_sessions(output: &str) -> Result<Vec<TmuxSessionSummary>, TmuxError> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_list_session_line)
        .collect()
}

fn parse_list_session_line(line: &str) -> Result<TmuxSessionSummary, TmuxError> {
    let mut parts = line.split('\t');
    let name = parts
        .next()
        .ok_or_else(|| TmuxError::MalformedListLine(line.into()))?;
    let windows = parts
        .next()
        .ok_or_else(|| TmuxError::MalformedListLine(line.into()))?;
    let attached = parts
        .next()
        .ok_or_else(|| TmuxError::MalformedListLine(line.into()))?;

    if parts.next().is_some() {
        return Err(TmuxError::MalformedListLine(line.into()));
    }

    Ok(TmuxSessionSummary {
        name: TmuxSessionName::new(name)?,
        windows: parse_u32("session_windows", windows)?,
        attached_clients: parse_u32("session_attached", attached)?,
    })
}

fn parse_u32(field: &'static str, value: &str) -> Result<u32, TmuxError> {
    value.parse().map_err(|_| TmuxError::InvalidInteger {
        field,
        value: value.into(),
    })
}

fn render_command(spec: &TmuxCommandSpec, socket_path: Option<&Path>) -> String {
    let mut parts = vec![spec.program.clone()];

    if let Some(socket_path) = socket_path {
        parts.push("-S".into());
        parts.push(socket_path.display().to_string());
    }

    parts.extend(spec.args.iter().cloned());
    parts.join(" ")
}

fn is_no_server_running(stderr: &str) -> bool {
    stderr.contains("no server running")
        || stderr.contains("failed to connect to server")
        || stderr.contains("error connecting to")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn new_session_command_is_stable_and_typed() {
        let session = TmuxSessionName::new("main").expect("session name should be valid");
        let command = build_new_session_command(&session, "/workspace");

        assert_eq!(command.program, "tmux");
        assert_eq!(
            command.args,
            vec!["new-session", "-d", "-s", "main", "-c", "/workspace"]
        );
    }

    #[test]
    fn parse_list_sessions_output() {
        let sessions =
            parse_list_sessions("main\t2\t1\nops\t1\t0\n").expect("list output should parse");

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name.as_str(), "main");
        assert_eq!(sessions[0].windows, 2);
        assert_eq!(sessions[0].attached_clients, 1);
        assert_eq!(sessions[1].name.as_str(), "ops");
    }

    #[test]
    fn session_name_rejects_unsupported_characters() {
        let error = TmuxSessionName::new("bad:name").expect_err("colon must be rejected");

        assert!(matches!(
            error,
            TmuxError::InvalidSessionName(value) if value == "bad:name"
        ));
    }

    #[test]
    fn malformed_list_line_is_rejected() {
        let error = parse_list_sessions("main\tbroken\n").expect_err("line must be malformed");

        assert!(matches!(
            error,
            TmuxError::MalformedListLine(value) if value == "main\tbroken"
        ));
    }

    #[test]
    fn kill_session_command_is_stable_and_typed() {
        let session = TmuxSessionName::new("main").expect("session name should be valid");
        let command = build_kill_session_command(&session);

        assert_eq!(command.program, "tmux");
        assert_eq!(command.args, vec!["kill-session", "-t", "main"]);
    }

    #[test]
    fn no_server_error_is_treated_as_empty_session_list() {
        assert!(is_no_server_running("no server running on /tmp/tmux.sock"));
        assert!(is_no_server_running("failed to connect to server"));
    }

    #[test]
    fn isolated_runtime_creates_and_lists_real_tmux_session() {
        let socket_path = test_socket_path();
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).expect("test socket dir should exist");
        }

        let runtime = TmuxRuntime::isolated(&socket_path);
        let session = TmuxSessionName::new("edex-test").expect("session name should be valid");

        runtime
            .create_session(&session, env!("CARGO_MANIFEST_DIR"))
            .expect("tmux session should be created");

        let sessions = runtime.list_sessions().expect("tmux sessions should list");
        assert!(sessions.iter().any(|item| item.name == session));

        runtime
            .kill_server()
            .expect("isolated tmux server should stop");
    }

    fn test_socket_path() -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time should move forward")
            .as_nanos();

        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join(format!("tmux-{stamp}.sock"))
    }
}
