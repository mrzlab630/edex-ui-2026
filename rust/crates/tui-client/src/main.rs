use anyhow::{anyhow, Context, Result};
use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::queue;
use crossterm::style::Print;
use crossterm::terminal::{
    self, disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen,
    LeaveAlternateScreen,
};
use runtime_api::{
    decode_json_frame, encode_json_frame, AgentPermissionMode, AgentProviderStatus, Command,
    ContextResult, ErrorCode, FileEntry, FileKind, FilePreview, FileSearchResult,
    HealthSnapshot, HistoryEntry, Query, RequestEnvelope, RequestPayload, ResponseEnvelope,
    ResponsePayload, Session, WorkspaceSummary,
};
use std::io::{BufRead, BufReader, Stdout, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

const HISTORY_LIMIT: usize = 20;
const FILE_LIMIT: usize = 40;

fn main() -> Result<()> {
    let mut app = App::new(DaemonClient::new(default_socket_path()));

    if std::env::args().any(|arg| arg == "--smoke") {
        app.bootstrap_from_env()?;
        app.refresh_all()?;
        println!(
            "TUI SMOKE OK workspaces={} sessions={} files={}",
            app.workspaces.len(),
            app.sessions.len(),
            app.files.len()
        );
        return Ok(());
    }

    let mut stdout = setup_terminal()?;
    let loop_result = run_app(&mut stdout, &mut app);
    restore_terminal(&mut stdout)?;
    loop_result
}

fn setup_terminal() -> Result<Stdout> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, Hide).context("enter alternate screen")?;
    Ok(stdout)
}

fn restore_terminal(stdout: &mut Stdout) -> Result<()> {
    disable_raw_mode().context("disable raw mode")?;
    execute!(stdout, Show, LeaveAlternateScreen).context("leave alternate screen")?;
    stdout.flush().context("flush terminal state")?;
    Ok(())
}

fn run_app(stdout: &mut Stdout, app: &mut App) -> Result<()> {
    app.refresh_all()?;

    loop {
        draw_app(stdout, app)?;
        if !event::poll(Duration::from_millis(200)).context("poll terminal events")? {
            continue;
        }

        let Event::Key(key) = event::read().context("read terminal event")? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        if app.handle_key(key.code)? {
            break;
        }
    }

    Ok(())
}

fn draw_app(stdout: &mut Stdout, app: &App) -> Result<()> {
    let (width, height) = terminal::size().context("read terminal size")?;
    let lines = app.render(width as usize, height as usize);

    queue!(stdout, MoveTo(0, 0), Clear(ClearType::All)).context("clear terminal")?;
    for (row, line) in lines.iter().enumerate() {
        queue!(stdout, MoveTo(0, row as u16), Print(line)).context("draw line")?;
    }
    stdout.flush().context("flush terminal")?;
    Ok(())
}

#[derive(Debug, Clone)]
struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    fn health(&self) -> Result<HealthSnapshot> {
        match self.request(RequestPayload::Query(Query::Health))? {
            ResponsePayload::Health(health) => Ok(health),
            payload => Err(unexpected_payload("Health", payload)),
        }
    }

    fn workspaces(&self) -> Result<Vec<WorkspaceSummary>> {
        match self.request(RequestPayload::Query(Query::Workspaces))? {
            ResponsePayload::Workspaces { workspaces } => Ok(workspaces),
            payload => Err(unexpected_payload("Workspaces", payload)),
        }
    }

    fn register_workspace(&self, root: &Path) -> Result<WorkspaceSummary> {
        let workspace_id = Uuid::new_v4();
        let root_text = root.display().to_string();
        let name = workspace_name_for_root(root);
        match self.request(RequestPayload::Command(Command::RegisterWorkspace {
            workspace_id,
            name: name.clone(),
            roots: vec![root_text.clone()],
        }))? {
            ResponsePayload::WorkspaceRegistered { .. } => Ok(WorkspaceSummary {
                id: workspace_id,
                name,
                roots: vec![root_text],
                bookmarks: Vec::new(),
            }),
            payload => Err(unexpected_payload("RegisterWorkspace", payload)),
        }
    }

    fn sessions(&self, workspace_id: Option<Uuid>) -> Result<Vec<Session>> {
        match self.request(RequestPayload::Query(Query::Sessions { workspace_id }))? {
            ResponsePayload::Sessions { sessions } => Ok(sessions),
            payload => Err(unexpected_payload("Sessions", payload)),
        }
    }

    fn recent_history(
        &self,
        workspace_id: Option<Uuid>,
        session_id: Option<Uuid>,
        limit: usize,
    ) -> Result<Vec<HistoryEntry>> {
        match self.request(RequestPayload::Query(Query::RecentHistory {
            workspace_id,
            session_id,
            limit,
        }))? {
            ResponsePayload::HistoryEntries { entries } => Ok(entries),
            payload => Err(unexpected_payload("RecentHistory", payload)),
        }
    }

    fn file_list(&self, workspace_id: Uuid, path: &Path, limit: usize) -> Result<Vec<FileEntry>> {
        match self.request(RequestPayload::Query(Query::FileList {
            workspace_id,
            path: path.display().to_string(),
            limit,
        }))? {
            ResponsePayload::FileEntries { entries } => Ok(entries),
            payload => Err(unexpected_payload("FileList", payload)),
        }
    }

    fn file_preview(
        &self,
        workspace_id: Uuid,
        path: &Path,
        max_bytes: usize,
        max_lines: usize,
    ) -> Result<FilePreview> {
        match self.request(RequestPayload::Query(Query::FilePreview {
            workspace_id,
            path: path.display().to_string(),
            max_bytes,
            max_lines,
        }))? {
            ResponsePayload::FilePreview { preview } => Ok(preview),
            payload => Err(unexpected_payload("FilePreview", payload)),
        }
    }

    fn file_search(
        &self,
        workspace_id: Uuid,
        root_path: &Path,
        text: String,
        limit: usize,
    ) -> Result<Vec<FileSearchResult>> {
        match self.request(RequestPayload::Query(Query::FileSearch {
            workspace_id,
            root_path: root_path.display().to_string(),
            text,
            limit,
        }))? {
            ResponsePayload::FileSearchResults { results } => Ok(results),
            payload => Err(unexpected_payload("FileSearch", payload)),
        }
    }

    fn context_search(
        &self,
        workspace_id: Uuid,
        session_id: Option<Uuid>,
        text: String,
        limit: usize,
    ) -> Result<Vec<ContextResult>> {
        match self.request(RequestPayload::Query(Query::ContextSearch {
            workspace_id: Some(workspace_id),
            session_id,
            text,
            limit,
        }))? {
            ResponsePayload::ContextResults { results } => Ok(results),
            payload => Err(unexpected_payload("ContextSearch", payload)),
        }
    }

    fn agent_provider_status(&self) -> Result<AgentProviderStatus> {
        match self.request(RequestPayload::Query(Query::AgentProviderStatus))? {
            ResponsePayload::AgentProviderStatus { status } => Ok(status),
            payload => Err(unexpected_payload("AgentProviderStatus", payload)),
        }
    }

    fn run_agent_task(
        &self,
        workspace_id: Uuid,
        session_id: Option<Uuid>,
        cwd: Option<&Path>,
        prompt: String,
    ) -> Result<String> {
        match self.request(RequestPayload::Command(Command::RunAgentTask {
            task_id: Uuid::new_v4(),
            workspace_id,
            session_id,
            cwd: cwd.map(|path| path.display().to_string()),
            prompt,
            model: None,
            context_query: None,
            context_limit: 8,
            permission_mode: AgentPermissionMode::WorkspaceWrite,
            allowed_tools: vec!["glob".into(), "grep".into(), "read".into()],
        }))? {
            ResponsePayload::AgentTaskCompleted { output, .. } => Ok(output),
            payload => Err(unexpected_payload("RunAgentTask", payload)),
        }
    }

    fn register_local_tmux_session(&self, workspace_id: Uuid, cwd: &Path) -> Result<Uuid> {
        let session_id = Uuid::new_v4();
        match self.request(RequestPayload::Command(Command::RegisterLocalTmuxSession {
            session_id,
            workspace_id,
            cwd: cwd.display().to_string(),
        }))? {
            ResponsePayload::SessionRegistered {
                session,
                ..
            } if session.id == session_id => Ok(session_id),
            payload => Err(unexpected_payload("RegisterLocalTmuxSession", payload)),
        }
    }

    fn remove_session(&self, session_id: Uuid) -> Result<()> {
        match self.request(RequestPayload::Command(Command::RemoveSession { session_id }))? {
            ResponsePayload::SessionRemoved { .. } => Ok(()),
            payload => Err(unexpected_payload("RemoveSession", payload)),
        }
    }

    fn request(&self, payload: RequestPayload) -> Result<ResponsePayload> {
        let request = RequestEnvelope::new(Uuid::new_v4().to_string(), payload);
        let encoded = encode_json_frame(&request).context("encode request frame")?;
        let mut stream =
            UnixStream::connect(&self.socket_path).with_context(|| self.socket_error("connect"))?;
        stream
            .write_all(&encoded)
            .with_context(|| self.socket_error("write request"))?;
        stream.flush().with_context(|| self.socket_error("flush request"))?;

        let mut line = String::new();
        let mut reader = BufReader::new(stream);
        reader
            .read_line(&mut line)
            .with_context(|| self.socket_error("read response"))?;
        let response: ResponseEnvelope = decode_json_frame(&line)
            .context("decode response frame")?
            .ok_or_else(|| anyhow!("daemon returned empty response"))?;
        if response.ok {
            Ok(response.payload)
        } else {
            match response.payload {
                ResponsePayload::Error(error) => {
                    Err(anyhow!("{}: {}", error_code(error.code), error.message))
                }
                payload => Err(unexpected_payload("ApiError", payload)),
            }
        }
    }

    fn socket_error(&self, action: &str) -> String {
        format!("{} `{}`", action, self.socket_path.display())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusPane {
    Workspaces,
    Sessions,
    Files,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    ContextSearch,
    FileSearch,
    AgentPrompt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryScope {
    Workspace,
    Session,
}

#[derive(Debug)]
struct App {
    client: DaemonClient,
    focus: FocusPane,
    input_mode: InputMode,
    health: Option<HealthSnapshot>,
    workspaces: Vec<WorkspaceSummary>,
    selected_workspace: usize,
    sessions: Vec<Session>,
    selected_session: usize,
    current_dir: Option<PathBuf>,
    files: Vec<FileEntry>,
    selected_file: usize,
    preview: Option<FilePreview>,
    history_scope: HistoryScope,
    history: Vec<HistoryEntry>,
    context_results: Vec<ContextResult>,
    file_search_results: Vec<FileSearchResult>,
    agent_status: Option<AgentProviderStatus>,
    agent_output: String,
    input_buffer: String,
    status_line: String,
}

impl App {
    fn new(client: DaemonClient) -> Self {
        Self {
            client,
            focus: FocusPane::Workspaces,
            input_mode: InputMode::Normal,
            health: None,
            workspaces: Vec::new(),
            selected_workspace: 0,
            sessions: Vec::new(),
            selected_session: 0,
            current_dir: None,
            files: Vec::new(),
            selected_file: 0,
            preview: None,
            history_scope: HistoryScope::Workspace,
            history: Vec::new(),
            context_results: Vec::new(),
            file_search_results: Vec::new(),
            agent_status: None,
            agent_output: String::new(),
            input_buffer: String::new(),
            status_line: String::from("Loading daemon state..."),
        }
    }

    fn refresh_all(&mut self) -> Result<()> {
        self.health = Some(self.client.health()?);
        self.agent_status = Some(self.client.agent_provider_status()?);
        self.workspaces = self.client.workspaces()?;
        self.clamp_workspace_selection();
        self.refresh_selected_workspace()?;
        self.status_line = String::from("Refreshed daemon state");
        Ok(())
    }

    fn bootstrap_from_env(&mut self) -> Result<()> {
        let root = match std::env::var("EDEX_TUI_BOOTSTRAP_ROOT") {
            Ok(root) if !root.trim().is_empty() => Some(PathBuf::from(root)),
            _ => None,
        };
        let create_tmux = std::env::var("EDEX_TUI_BOOTSTRAP_TMUX")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        if let Some(root) = root {
            self.bootstrap_workspace(root)?;
            if create_tmux {
                self.create_tmux_session()?;
            }
        }

        Ok(())
    }

    fn refresh_selected_workspace(&mut self) -> Result<()> {
        let Some(workspace) = self.selected_workspace().cloned() else {
            self.sessions.clear();
            self.files.clear();
            self.history.clear();
            self.preview = None;
            self.context_results.clear();
            self.file_search_results.clear();
            self.current_dir = None;
            self.history_scope = HistoryScope::Workspace;
            return Ok(());
        };

        self.sessions = self.client.sessions(Some(workspace.id))?;
        self.clamp_session_selection();
        self.refresh_history()?;

        let next_dir = match self.current_dir.clone() {
            Some(path) if path_is_within_workspace(&path, &workspace) => path,
            _ => workspace_root(&workspace)?,
        };
        self.current_dir = Some(next_dir.clone());
        self.files = self.client.file_list(workspace.id, &next_dir, FILE_LIMIT)?;
        self.clamp_file_selection();
        self.refresh_preview()?;
        Ok(())
    }

    fn refresh_history(&mut self) -> Result<()> {
        let Some(workspace) = self.selected_workspace().cloned() else {
            self.history.clear();
            return Ok(());
        };
        let session_id = self.active_session_scope_id();
        self.history = self
            .client
            .recent_history(Some(workspace.id), session_id, HISTORY_LIMIT)?;
        Ok(())
    }

    fn refresh_preview(&mut self) -> Result<()> {
        let Some(workspace) = self.selected_workspace().cloned() else {
            self.preview = None;
            return Ok(());
        };
        let Some(file) = self.selected_file_entry().cloned() else {
            self.preview = None;
            return Ok(());
        };
        if file.kind != FileKind::File {
            self.preview = None;
            return Ok(());
        }
        self.preview = Some(self.client.file_preview(
            workspace.id,
            Path::new(&file.path),
            8 * 1024,
            80,
        )?);
        Ok(())
    }

    fn handle_key(&mut self, key: KeyCode) -> Result<bool> {
        if self.input_mode != InputMode::Normal {
            return self.handle_input_mode(key);
        }

        match key {
            KeyCode::Char('q') => return Ok(true),
            KeyCode::Tab => self.focus = self.focus.next(),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1)?,
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1)?,
            KeyCode::Enter => self.activate_selection()?,
            KeyCode::Backspace => self.navigate_parent()?,
            KeyCode::Char('r') => self.refresh_all()?,
            KeyCode::Char('w') => self.bootstrap_workspace(default_bootstrap_root())?,
            KeyCode::Char('n') => self.create_tmux_session()?,
            KeyCode::Char('x') => self.remove_selected_session()?,
            KeyCode::Char('h') => self.toggle_history_scope()?,
            KeyCode::Char('g') => self.jump_to_workspace_root()?,
            KeyCode::Char('b') => self.jump_to_next_target()?,
            KeyCode::Char('o') => self.open_top_file_search_result()?,
            KeyCode::Char('f') => {
                self.input_mode = InputMode::FileSearch;
                self.input_buffer.clear();
                self.status_line = String::from("File search mode");
            }
            KeyCode::Char('c') => {
                self.context_results.clear();
                self.file_search_results.clear();
                self.status_line = String::from("Cleared transient search results");
            }
            KeyCode::Char('/') => {
                self.input_mode = InputMode::ContextSearch;
                self.input_buffer.clear();
                self.status_line = String::from("Context search mode");
            }
            KeyCode::Char('a') => {
                self.input_mode = InputMode::AgentPrompt;
                self.input_buffer.clear();
                self.status_line = String::from("Agent prompt mode");
            }
            _ => {}
        }

        Ok(false)
    }

    fn handle_input_mode(&mut self, key: KeyCode) -> Result<bool> {
        match key {
            KeyCode::Esc => {
                self.input_mode = InputMode::Normal;
                self.input_buffer.clear();
                self.status_line = String::from("Cancelled input mode");
            }
            KeyCode::Enter => {
                let text = self.input_buffer.trim().to_owned();
                let mode = self.input_mode;
                self.input_mode = InputMode::Normal;
                self.input_buffer.clear();
                if text.is_empty() {
                    self.status_line = String::from("Input ignored: empty buffer");
                } else {
                    match mode {
                        InputMode::ContextSearch => self.submit_context_search(text)?,
                        InputMode::FileSearch => self.submit_file_search(text)?,
                        InputMode::AgentPrompt => self.submit_agent_prompt(text)?,
                        InputMode::Normal => {}
                    }
                }
            }
            KeyCode::Backspace => {
                self.input_buffer.pop();
            }
            KeyCode::Char(ch) => self.input_buffer.push(ch),
            _ => {}
        }

        Ok(false)
    }

    fn submit_context_search(&mut self, text: String) -> Result<()> {
        let Some(workspace) = self.selected_workspace().cloned() else {
            self.status_line = String::from("No workspace selected");
            return Ok(());
        };
        self.file_search_results.clear();
        let scope = self.history_scope_label();
        self.context_results = self
            .client
            .context_search(workspace.id, self.active_session_scope_id(), text.clone(), 10)?;
        self.status_line = format!("Context search completed in {scope} scope: `{text}`");
        Ok(())
    }

    fn submit_file_search(&mut self, text: String) -> Result<()> {
        let Some(workspace) = self.selected_workspace().cloned() else {
            self.status_line = String::from("No workspace selected");
            return Ok(());
        };
        let root = self
            .current_dir
            .clone()
            .or_else(|| workspace_root(&workspace).ok())
            .ok_or_else(|| anyhow!("workspace `{}` has no root", workspace.name))?;
        self.context_results.clear();
        self.file_search_results = self.client.file_search(workspace.id, &root, text.clone(), 12)?;
        self.status_line = format!("File search completed in {}: `{text}`", root.display());
        Ok(())
    }

    fn submit_agent_prompt(&mut self, prompt: String) -> Result<()> {
        let Some(workspace) = self.selected_workspace().cloned() else {
            self.status_line = String::from("No workspace selected");
            return Ok(());
        };
        let cwd = self.current_dir.as_deref();
        let session_scope = self.active_session_scope_id();
        let scope_label = if session_scope.is_some() {
            "session"
        } else {
            "workspace"
        };
        self.agent_output = self
            .client
            .run_agent_task(workspace.id, session_scope, cwd, prompt)?;
        self.history_scope = HistoryScope::Workspace;
        self.status_line = format!("Agent task completed in {scope_label} scope");
        self.refresh_history()?;
        Ok(())
    }

    fn bootstrap_workspace(&mut self, root: PathBuf) -> Result<()> {
        let canonical_root = root
            .canonicalize()
            .with_context(|| format!("canonicalize bootstrap root `{}`", root.display()))?;
        let existing = self
            .workspaces
            .iter()
            .find(|workspace| workspace.roots.iter().any(|candidate| Path::new(candidate) == canonical_root))
            .cloned();

        let workspace = if let Some(workspace) = existing {
            workspace
        } else {
            self.client.register_workspace(&canonical_root)?
        };

        self.refresh_all()?;
        if let Some(index) = self.workspaces.iter().position(|candidate| candidate.id == workspace.id) {
            self.selected_workspace = index;
            self.history_scope = HistoryScope::Workspace;
            self.context_results.clear();
            self.file_search_results.clear();
            self.refresh_selected_workspace()?;
        }
        self.status_line = format!("Workspace ready: {}", canonical_root.display());
        Ok(())
    }

    fn create_tmux_session(&mut self) -> Result<()> {
        let Some(workspace) = self.selected_workspace().cloned() else {
            self.status_line = String::from("No workspace selected");
            return Ok(());
        };
        let cwd = self
            .current_dir
            .clone()
            .or_else(|| workspace_root(&workspace).ok())
            .ok_or_else(|| anyhow!("workspace `{}` has no root", workspace.name))?;
        let session_id = self.client.register_local_tmux_session(workspace.id, &cwd)?;
        self.refresh_selected_workspace()?;
        if let Some(index) = self.sessions.iter().position(|session| session.id == session_id) {
            self.selected_session = index;
        }
        self.history_scope = HistoryScope::Session;
        self.context_results.clear();
        self.file_search_results.clear();
        self.refresh_history()?;
        self.status_line = format!("Local tmux session created: {session_id}");
        Ok(())
    }

    fn remove_selected_session(&mut self) -> Result<()> {
        let Some(session) = self.selected_session().cloned() else {
            self.status_line = String::from("No session selected");
            return Ok(());
        };
        self.client.remove_session(session.id)?;
        self.refresh_selected_workspace()?;
        if self.sessions.is_empty() {
            self.history_scope = HistoryScope::Workspace;
        }
        self.context_results.clear();
        self.file_search_results.clear();
        self.refresh_history()?;
        self.status_line = format!("Session removed: {}", session.id);
        Ok(())
    }

    fn toggle_history_scope(&mut self) -> Result<()> {
        self.history_scope = match self.history_scope {
            HistoryScope::Workspace => {
                if self.selected_session().is_some() {
                    HistoryScope::Session
                } else {
                    HistoryScope::Workspace
                }
            }
            HistoryScope::Session => HistoryScope::Workspace,
        };
        self.context_results.clear();
        self.file_search_results.clear();
        self.refresh_history()?;
        self.status_line = format!("History scope: {}", self.history_scope_label());
        Ok(())
    }

    fn jump_to_workspace_root(&mut self) -> Result<()> {
        let Some(workspace) = self.selected_workspace().cloned() else {
            self.status_line = String::from("No workspace selected");
            return Ok(());
        };
        let root = workspace_root(&workspace)?;
        self.jump_to_path(root)?;
        self.status_line = format!("Jumped to workspace root: {}", self.current_dir_display());
        Ok(())
    }

    fn jump_to_next_target(&mut self) -> Result<()> {
        let Some(workspace) = self.selected_workspace().cloned() else {
            self.status_line = String::from("No workspace selected");
            return Ok(());
        };
        let targets = workspace_targets(&workspace);
        if targets.is_empty() {
            self.status_line = String::from("Workspace has no roots or bookmarks");
            return Ok(());
        }

        let next = match self.current_dir.as_ref() {
            Some(current) => {
                let current = current.as_path();
                let current_index = targets.iter().position(|target| target == current);
                match current_index {
                    Some(index) => targets[(index + 1) % targets.len()].clone(),
                    None => targets[0].clone(),
                }
            }
            None => targets[0].clone(),
        };

        self.jump_to_path(next)?;
        self.status_line = format!("Jumped to target: {}", self.current_dir_display());
        Ok(())
    }

    fn open_top_file_search_result(&mut self) -> Result<()> {
        let Some(result) = self.file_search_results.first().cloned() else {
            self.status_line = String::from("No file search results to open");
            return Ok(());
        };
        let path = PathBuf::from(&result.entry.path);
        let target = if result.entry.kind == FileKind::Directory {
            path
        } else {
            path.parent()
                .map(Path::to_path_buf)
                .unwrap_or(path)
        };
        self.jump_to_path(target)?;

        if result.entry.kind != FileKind::Directory {
            if let Some(index) = self
                .files
                .iter()
                .position(|entry| entry.path == result.entry.path)
            {
                self.selected_file = index;
                self.refresh_preview()?;
            }
        }

        self.status_line = format!("Opened search result: {}", result.entry.path);
        Ok(())
    }

    fn move_selection(&mut self, delta: isize) -> Result<()> {
        match self.focus {
            FocusPane::Workspaces => {
                self.selected_workspace =
                    shift_index(self.selected_workspace, self.workspaces.len(), delta);
                self.current_dir = None;
                self.history_scope = HistoryScope::Workspace;
                self.context_results.clear();
                self.file_search_results.clear();
                self.refresh_selected_workspace()?;
            }
            FocusPane::Sessions => {
                self.selected_session = shift_index(self.selected_session, self.sessions.len(), delta);
                if self.history_scope == HistoryScope::Session {
                    self.context_results.clear();
                    self.file_search_results.clear();
                    self.refresh_history()?;
                }
            }
            FocusPane::Files => {
                self.selected_file = shift_index(self.selected_file, self.files.len(), delta);
                self.refresh_preview()?;
            }
        }
        Ok(())
    }

    fn activate_selection(&mut self) -> Result<()> {
        match self.focus {
            FocusPane::Workspaces => {
                self.current_dir = None;
                self.history_scope = HistoryScope::Workspace;
                self.context_results.clear();
                self.file_search_results.clear();
                self.refresh_selected_workspace()?;
            }
            FocusPane::Sessions => {
                let Some(session_id) = self.selected_session().map(|session| session.id) else {
                    return Ok(());
                };
                self.history_scope = HistoryScope::Session;
                self.context_results.clear();
                self.file_search_results.clear();
                self.refresh_history()?;
                self.status_line = format!("Selected session `{session_id}`");
            }
            FocusPane::Files => {
                let Some(file) = self.selected_file_entry().cloned() else {
                    return Ok(());
                };
                if file.kind == FileKind::Directory {
                    let Some(workspace) = self.selected_workspace().cloned() else {
                        return Ok(());
                    };
                    self.current_dir = Some(PathBuf::from(&file.path));
                    self.files = self.client.file_list(workspace.id, Path::new(&file.path), FILE_LIMIT)?;
                    self.selected_file = 0;
                    self.refresh_preview()?;
                    self.status_line = format!("Entered {}", file.path);
                } else {
                    self.refresh_preview()?;
                }
            }
        }
        Ok(())
    }

    fn navigate_parent(&mut self) -> Result<()> {
        if self.focus != FocusPane::Files {
            return Ok(());
        }
        let Some(workspace) = self.selected_workspace().cloned() else {
            return Ok(());
        };
        let Some(current_dir) = self.current_dir.clone() else {
            return Ok(());
        };
        let Some(parent) = current_dir.parent() else {
            return Ok(());
        };
        if !path_is_within_workspace(parent, &workspace) {
            self.status_line = String::from("Already at workspace root");
            return Ok(());
        }
        self.current_dir = Some(parent.to_path_buf());
        self.files = self.client.file_list(workspace.id, parent, FILE_LIMIT)?;
        self.selected_file = 0;
        self.refresh_preview()?;
        self.status_line = format!("Moved to {}", parent.display());
        Ok(())
    }

    fn jump_to_path(&mut self, path: PathBuf) -> Result<()> {
        let Some(workspace) = self.selected_workspace().cloned() else {
            return Ok(());
        };
        let path = path
            .canonicalize()
            .with_context(|| format!("canonicalize jump target `{}`", path.display()))?;
        if !path_is_within_workspace(&path, &workspace) {
            return Err(anyhow!(
                "jump target `{}` is outside workspace",
                path.display()
            ));
        }
        self.current_dir = Some(path.clone());
        self.files = self.client.file_list(workspace.id, &path, FILE_LIMIT)?;
        self.selected_file = 0;
        self.focus = FocusPane::Files;
        self.refresh_preview()?;
        Ok(())
    }

    fn render(&self, width: usize, height: usize) -> Vec<String> {
        if width < 40 || height < 12 {
            return vec![
                clip_line("Terminal too small for eDEX TUI client", width),
                clip_line("Resize the terminal and retry.", width),
            ];
        }

        let header = clip_line(&self.header_line(), width);
        let footer = clip_line(&self.footer_line(), width);
        let body_height = height.saturating_sub(2);
        let columns = split_width(width, &[25, 35, 40]);
        let left = split_height(body_height, &[45, 55]);
        let center = split_height(body_height, &[60, 40]);
        let right = split_height(body_height, &[55, 45]);

        let left_column = stack_vertical(vec![
            pane(
                "Workspaces",
                self.workspaces
                    .iter()
                    .enumerate()
                    .map(|(index, workspace)| selectable(index == self.selected_workspace, &workspace.name))
                    .collect(),
                columns[0],
                left[0],
                self.focus == FocusPane::Workspaces,
            ),
            pane(
                "Sessions",
                self.sessions
                    .iter()
                    .enumerate()
                    .map(|(index, session)| {
                        selectable(
                            index == self.selected_session,
                            &format!("{:?} {}", session.kind, session.cwd.display()),
                        )
                    })
                    .collect(),
                columns[0],
                left[1],
                self.focus == FocusPane::Sessions,
            ),
        ]);

        let center_column = stack_vertical(vec![
            pane(
                &format!(
                    "Files {}",
                    self.current_dir
                        .as_ref()
                        .map(|path| format!("({})", path.display()))
                        .unwrap_or_default()
                ),
                self.files
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| {
                        selectable(
                            index == self.selected_file,
                            &format!("{} {}", file_icon(entry.kind), entry.name),
                        )
                    })
                    .collect(),
                columns[1],
                center[0],
                self.focus == FocusPane::Files,
            ),
            pane(
                "Preview",
                self.preview_lines(),
                columns[1],
                center[1],
                false,
            ),
        ]);

        let right_column = stack_vertical(vec![
            pane(
                &self.history_panel_title(),
                self.history_or_context_lines(),
                columns[2],
                right[0],
                false,
            ),
            pane("Agent", self.agent_lines(), columns[2], right[1], false),
        ]);

        let body = join_columns(vec![left_column, center_column, right_column]);
        let mut lines = Vec::with_capacity(height);
        lines.push(header);
        lines.extend(body.into_iter().take(body_height));
        while lines.len() < height.saturating_sub(1) {
            lines.push(" ".repeat(width));
        }
        lines.push(footer);
        lines
    }

    fn header_line(&self) -> String {
        let socket = self
            .health
            .as_ref()
            .map(|health| health.daemon.socket_path.clone())
            .unwrap_or_else(|| self.client.socket_path.display().to_string());
        let provider = self
            .agent_status
            .as_ref()
            .map(|status| {
                if status.available {
                    format!("AI:{}:ready", status.provider)
                } else {
                    format!("AI:{}:down", status.provider)
                }
            })
            .unwrap_or_else(|| String::from("AI:unknown"));
        format!(
            "eDEX-UI 2026 TUI | socket={} | {} | focus={:?} | scope={}",
            socket,
            provider,
            self.focus,
            self.history_scope_label()
        )
    }

    fn footer_line(&self) -> String {
        let mode = match self.input_mode {
            InputMode::Normal => "NORMAL",
            InputMode::ContextSearch => "CONTEXT",
            InputMode::FileSearch => "FILES",
            InputMode::AgentPrompt => "AGENT",
        };
        let input_suffix = if self.input_mode == InputMode::Normal {
            String::new()
        } else {
            format!(" | input: {}", self.input_buffer)
        };
        format!(
            "{} | {} | scope={} | q quit | Tab cycle | Enter act | Backspace parent | r refresh | w workspace | n tmux | x remove | h scope | g root | b targets | o open-hit | / context | f files | c clear | a agent{}",
            mode,
            self.status_line,
            self.history_scope_label(),
            input_suffix
        )
    }

    fn preview_lines(&self) -> Vec<String> {
        match self.preview.as_ref() {
            Some(preview) => preview
                .content
                .clone()
                .unwrap_or_else(|| {
                    format!(
                        "non-text or redacted file\nsensitivity={:?}\nsize={} bytes",
                        preview.sensitivity, preview.size_bytes
                    )
                })
                .lines()
                .map(ToOwned::to_owned)
                .collect(),
            None => vec![String::from("No file preview")],
        }
    }

    fn history_or_context_lines(&self) -> Vec<String> {
        if !self.file_search_results.is_empty() {
            self.file_search_results
                .iter()
                .map(|result| {
                    format!(
                        "[{}] {} {}",
                        result.score,
                        file_icon(result.entry.kind),
                        result.entry.path
                    )
                })
                .collect()
        } else if !self.context_results.is_empty() {
            self.context_results
                .iter()
                .map(|result| format!("[{:.2}] {:?} {}", result.score, result.kind, result.preview))
                .collect()
        } else {
            self.history
                .iter()
                .map(|entry| format!("{:?}: {}", entry.kind, entry.content))
                .collect()
        }
    }

    fn history_panel_title(&self) -> String {
        if !self.file_search_results.is_empty() {
            String::from("File Search")
        } else if !self.context_results.is_empty() {
            format!("Context ({})", self.history_scope_label())
        } else {
            format!("History ({})", self.history_scope_label())
        }
    }

    fn agent_lines(&self) -> Vec<String> {
        let provider = self
            .agent_status
            .as_ref()
            .map(|status| {
                format!(
                    "provider={}\navailable={}\nversion={}\nagent_scope={}",
                    status.provider,
                    status.available,
                    status.version_report.as_deref().unwrap_or("n/a"),
                    self.history_scope_label(),
                )
            })
            .unwrap_or_else(|| String::from("provider status unavailable"));
        let session_meta = self.selected_session().map(|session| {
            format!(
                "session_id={}\nkind={:?}\nbacking={:?}\ncwd={}",
                session.id,
                session.kind,
                session.backing,
                session.cwd.display()
            )
        });
        let base = match session_meta {
            Some(meta) => format!("{provider}\n\n{meta}"),
            None => provider,
        };
        let body = if self.agent_output.trim().is_empty() {
            base
        } else {
            format!("{base}\n\n{}", self.agent_output)
        };
        body.lines().map(ToOwned::to_owned).collect()
    }

    fn selected_workspace(&self) -> Option<&WorkspaceSummary> {
        self.workspaces.get(self.selected_workspace)
    }

    fn active_session_scope_id(&self) -> Option<Uuid> {
        match self.history_scope {
            HistoryScope::Workspace => None,
            HistoryScope::Session => self.selected_session().map(|session| session.id),
        }
    }

    fn history_scope_label(&self) -> &'static str {
        match self.history_scope {
            HistoryScope::Workspace => "workspace",
            HistoryScope::Session => "session",
        }
    }

    fn current_dir_display(&self) -> String {
        self.current_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| String::from("<none>"))
    }

    fn selected_session(&self) -> Option<&Session> {
        self.sessions.get(self.selected_session)
    }

    fn selected_file_entry(&self) -> Option<&FileEntry> {
        self.files.get(self.selected_file)
    }

    fn clamp_workspace_selection(&mut self) {
        if self.workspaces.is_empty() {
            self.selected_workspace = 0;
        } else if self.selected_workspace >= self.workspaces.len() {
            self.selected_workspace = self.workspaces.len() - 1;
        }
    }

    fn clamp_session_selection(&mut self) {
        if self.sessions.is_empty() {
            self.selected_session = 0;
        } else if self.selected_session >= self.sessions.len() {
            self.selected_session = self.sessions.len() - 1;
        }
    }

    fn clamp_file_selection(&mut self) {
        if self.files.is_empty() {
            self.selected_file = 0;
        } else if self.selected_file >= self.files.len() {
            self.selected_file = self.files.len() - 1;
        }
    }
}

impl FocusPane {
    fn next(self) -> Self {
        match self {
            FocusPane::Workspaces => FocusPane::Sessions,
            FocusPane::Sessions => FocusPane::Files,
            FocusPane::Files => FocusPane::Workspaces,
        }
    }
}

fn pane(title: &str, lines: Vec<String>, width: usize, height: usize, focused: bool) -> Vec<String> {
    if width < 4 || height < 3 {
        return vec![" ".repeat(width); height];
    }

    let mut output = Vec::with_capacity(height);
    let label = if focused {
        format!("* {title}")
    } else {
        title.to_owned()
    };
    output.push(border_line(&label, width));
    let inner_width = width.saturating_sub(2);
    let inner_height = height.saturating_sub(2);

    for line in lines.into_iter().take(inner_height) {
        output.push(format!("|{}|", pad_str(&line, inner_width)));
    }
    while output.len() < height.saturating_sub(1) {
        output.push(format!("|{}|", " ".repeat(inner_width)));
    }
    output.push(format!("+{}+", "-".repeat(width.saturating_sub(2))));
    output
}

fn border_line(title: &str, width: usize) -> String {
    let inner_width = width.saturating_sub(2);
    let title = format!(" {title} ");
    let clipped = clip_line(&title, inner_width);
    let dashes = inner_width.saturating_sub(display_width(&clipped));
    format!("+{}{}+", clipped, "-".repeat(dashes))
}

fn join_columns(columns: Vec<Vec<String>>) -> Vec<String> {
    let height = columns.iter().map(Vec::len).max().unwrap_or(0);
    let widths = columns
        .iter()
        .map(|column| column.first().map(|line| display_width(line)).unwrap_or(0))
        .collect::<Vec<_>>();

    let mut result = Vec::with_capacity(height);
    for row in 0..height {
        let mut line = String::new();
        for (index, column) in columns.iter().enumerate() {
            if index > 0 {
                line.push(' ');
            }
            if let Some(cell) = column.get(row) {
                line.push_str(cell);
            } else {
                line.push_str(&" ".repeat(widths[index]));
            }
        }
        result.push(line);
    }
    result
}

fn stack_vertical(sections: Vec<Vec<String>>) -> Vec<String> {
    let mut result = Vec::new();
    for section in sections {
        result.extend(section);
    }
    result
}

fn split_width(total: usize, ratios: &[usize]) -> Vec<usize> {
    split_sizes(total, ratios)
}

fn split_height(total: usize, ratios: &[usize]) -> Vec<usize> {
    split_sizes(total, ratios)
}

fn split_sizes(total: usize, ratios: &[usize]) -> Vec<usize> {
    let sum = ratios.iter().sum::<usize>().max(1);
    let mut sizes = ratios
        .iter()
        .map(|ratio| total.saturating_mul(*ratio) / sum)
        .collect::<Vec<_>>();
    let assigned = sizes.iter().sum::<usize>();
    let mut remaining = total.saturating_sub(assigned);
    let mut index = 0;
    while remaining > 0 && !sizes.is_empty() {
        let len = sizes.len();
        let slot = index % len;
        sizes[slot] += 1;
        remaining -= 1;
        index += 1;
    }
    sizes
}

fn selectable(selected: bool, content: &str) -> String {
    if selected {
        format!("> {content}")
    } else {
        format!("  {content}")
    }
}

fn shift_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }

    let last = len.saturating_sub(1) as isize;
    (current as isize + delta).clamp(0, last) as usize
}

fn workspace_root(workspace: &WorkspaceSummary) -> Result<PathBuf> {
    workspace
        .roots
        .first()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("workspace `{}` has no roots", workspace.name))
}

fn workspace_targets(workspace: &WorkspaceSummary) -> Vec<PathBuf> {
    let mut targets = workspace
        .roots
        .iter()
        .chain(workspace.bookmarks.iter())
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    targets
}

fn path_is_within_workspace(path: &Path, workspace: &WorkspaceSummary) -> bool {
    workspace
        .roots
        .iter()
        .map(Path::new)
        .any(|root| path.starts_with(root))
}

fn file_icon(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Directory => "[D]",
        FileKind::File => "[F]",
        FileKind::Symlink => "[L]",
        FileKind::Other => "[?]",
    }
}

fn default_socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("EDEX_CORE_SOCKET") {
        return PathBuf::from(path);
    }
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("edex-ui-2026-rust-core.sock");
    }
    std::env::temp_dir().join("edex-ui-2026-rust-core.sock")
}

fn default_bootstrap_root() -> PathBuf {
    std::env::var("EDEX_TUI_BOOTSTRAP_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir())
        })
}

fn workspace_name_for_root(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| String::from("workspace"))
}

fn error_code(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::InvalidRequest => "invalid_request",
        ErrorCode::ValidationFailed => "validation_failed",
        ErrorCode::UnsupportedVersion => "unsupported_version",
        ErrorCode::NotFound => "not_found",
        ErrorCode::Busy => "busy",
        ErrorCode::Internal => "internal",
    }
}

fn unexpected_payload(expected: &str, payload: ResponsePayload) -> anyhow::Error {
    anyhow!("unexpected payload for {expected}: {payload:?}")
}

fn clip_line(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let mut clipped = String::new();
    for ch in text.chars() {
        if display_width(&clipped) + ch.len_utf8().min(1) > width {
            break;
        }
        clipped.push(ch);
    }
    pad_str(&clipped, width)
}

fn pad_str(text: &str, width: usize) -> String {
    let visible = display_width(text);
    if visible >= width {
        text.chars().take(width).collect()
    } else {
        format!("{text}{}", " ".repeat(width - visible))
    }
}

fn display_width(text: &str) -> usize {
    text.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_sizes_assigns_entire_total() {
        let sizes = split_sizes(17, &[25, 35, 40]);

        assert_eq!(sizes.iter().sum::<usize>(), 17);
        assert_eq!(sizes.len(), 3);
    }

    #[test]
    fn path_within_workspace_respects_roots() {
        let workspace = WorkspaceSummary {
            id: Uuid::new_v4(),
            name: String::from("main"),
            roots: vec![String::from("/tmp/demo")],
            bookmarks: Vec::new(),
        };

        assert!(path_is_within_workspace(Path::new("/tmp/demo/src"), &workspace));
        assert!(!path_is_within_workspace(Path::new("/tmp/other"), &workspace));
    }

    #[test]
    fn pane_keeps_requested_dimensions() {
        let rendered = pane(
            "Files",
            vec![String::from("> alpha"), String::from("  beta")],
            24,
            6,
            true,
        );

        assert_eq!(rendered.len(), 6);
        assert!(rendered.iter().all(|line| display_width(line) == 24));
    }
}
