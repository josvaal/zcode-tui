//! Estado de la aplicación y bucle de eventos.

use tokio::sync::mpsc;

use crate::{
    agent::{AgentEvent, Connector, ModelInfo, SessionInfo},
    theme::Theme,
    ui,
};

/// Etapas del picker de modelos.
#[derive(Clone)]
pub enum Picker {
    Models(usize),
    Levels(crate::agent::ModelInfo, usize),
    /// modo de colaboración: build/edit/plan/yolo
    Mode(usize),
}

/// Modals con búsqueda difusa, estilo opencode.
#[derive(Clone)]
pub enum Modal {
    /// `/` paleta de comandos
    Commands { sel: usize, query: String },
    /// `@` archivos del workspace
    Files { sel: usize, query: String },
    /// `$` skills
    Skills { sel: usize, query: String },
}

pub struct Command {
    pub name: &'static str,
    pub hint: &'static str,
}

pub const COMMANDS: [Command; 10] = [
    Command { name: "modelo", hint: "elegir plan/modelo/nivel (ctrl+p)" },
    Command { name: "modo", hint: "build/edit/plan/yolo (ctrl+o)" },
    Command { name: "sesiones", hint: "listar y reanudar (ctrl+s)" },
    Command { name: "tema", hint: "ciclar tema (ctrl+t)" },
    Command { name: "tools", hint: "expandir/colapsar tools y diffs (o)" },
    Command { name: "archivos", hint: "buscar archivo del workspace (@)" },
    Command { name: "fork", hint: "bifurcar la sesión con su historial" },
    Command { name: "compact", hint: "compactar el contexto de la sesión" },
    Command { name: "limpiar", hint: "vaciar el transcript" },
    Command { name: "salir", hint: "cerrar la TUI" },
];

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Role {
    User,
    Assistant,
    Tool,
    Diff,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MsgKind {
    Text,
    Reasoning,
}

pub struct Message {
    pub role: Role,
    pub kind: MsgKind,
    pub content: String,
    /// Tool/Diff: mostrar cuerpo completo o solo la línea compacta `▸ …`
    pub expanded: bool,
}

/// Cursor del input multilinea (posición en chars dentro de cada línea).
#[derive(Default, Clone, Copy)]
pub struct InputCursor {
    pub col: usize,
    pub row: usize,
}

/// Pregunta de userInput activa con el estado de selección.
pub struct UserInputPending {
    pub token: u64,
    pub questions: Vec<crate::agent::UiQuestion>,
    pub question_idx: usize,
    pub option_idx: usize,
}

pub struct App {
    pub theme: Theme,
    pub streaming_since: Option<std::time::Instant>,
    pub hint_shown: bool,
    pub messages: Vec<Message>,
    /// input multilinea: una String por línea
    pub input: Vec<String>,
    pub input_cursor: InputCursor,
    pub scroll: u16,
    pub auto_scroll: bool,
    pub focus_input: bool,
    pub streaming: bool,
    /// tick para el spinner del status bar (avanza con el redibujo de 50ms)
    pub tick: usize,
    /// prompts encolados mientras el agente trabaja
    pub queue: Vec<String>,
    /// modo de colaboración actual: build | edit | plan | yolo
    pub mode: String,
    /// uso de tokens de la sesión (de session/usage)
    pub usage_text: Option<String>,
    /// AskUserQuestion pendiente: (token, pregunta actual, opción elegida por pregunta)
    pub pending_user_input: Option<UserInputPending>,
    /// buffer de renombrado de sesión (sidebar, tecla r)
    pub rename: Option<String>,
    /// confirmación de borrado en la sidebar (segundo `d`)
    pub delete_armed: bool,
    pub connector: Connector,
    pub user_label: String,
    pub should_quit: bool,
    pub pending_permission: Option<(u64, String)>, // (token, summary)
    pub mouse_capture: bool,
    pub models: Vec<ModelInfo>,
    /// picker abierto: lista de modelos o selector de nivel para uno elegido
    pub picker: Option<Picker>,
    /// modal abierto: comandos /, archivos @, skills $
    pub modal: Option<Modal>,
    /// archivos del workspace (lazy, para el modal @)
    pub files: Vec<String>,
    /// skills descubiertas (lazy, para el modal $)
    pub skills: Vec<crate::modal::SkillInfo>,
    /// workspace raíz (descubrimiento de archivos/skills)
    pub workspace: std::path::PathBuf,
    pub sessions: Vec<SessionInfo>,
    /// sidebar de sesiones abierta con índice seleccionado
    pub sidebar: Option<usize>,
    pub current_session: Option<String>,
    pub reasoning_levels: Vec<String>,
    /// toolCallId → índice del mensaje Tool correspondiente
    tool_msg_index: std::collections::HashMap<String, usize>,
    /// hubo actividad de tools/permisos desde el último prompt
    activity_since_prompt: bool,
    /// thinking pendiente de revelar (efecto typewriter ~400 chars/s)
    anim_buffer: String,
    anim_last: std::time::Instant,
    event_tx: Option<mpsc::UnboundedSender<AgentEvent>>,
}

impl App {
    pub fn new(connector: Connector, workspace: String) -> App {
        App {
            theme: crate::theme::load(),
            streaming_since: None,
            hint_shown: false,
            messages: Vec::new(), // transcript vacío → pantalla de bienvenida
            input: vec![String::new()],
            input_cursor: InputCursor::default(),
            scroll: 0,
            auto_scroll: true,
            focus_input: true,
            streaming: false,
            tick: 0,
            queue: Vec::new(),
            mode: "build".into(),
            usage_text: None,
            pending_user_input: None,
            rename: None,
            delete_armed: false,
            connector,
            user_label: "tú".into(),
            should_quit: false,
            pending_permission: None,
            mouse_capture: false,
            tool_msg_index: Default::default(),
            activity_since_prompt: false,
            models: Vec::new(),
            picker: None,
            modal: None,
            files: Vec::new(),
            skills: Vec::new(),
            workspace: std::path::PathBuf::from(&workspace),
            sessions: Vec::new(),
            sidebar: None,
            current_session: None,
            reasoning_levels: vec!["low".into(), "high".into(), "max".into()],
            anim_buffer: String::new(),
            anim_last: std::time::Instant::now(),
            event_tx: None,
        }
    }

    /// Revela el thinking del buffer a ~400 chars/s (efecto typewriter).
    fn drain_anim_buffer(&mut self) {
        if self.anim_buffer.is_empty() {
            return;
        }
        let elapsed = self.anim_last.elapsed().as_millis() as usize;
        let to_reveal = (elapsed * 400 / 1000).max(1);
        let take: usize = self.anim_buffer.chars().count().min(to_reveal);
        let reveal: String = self.anim_buffer.chars().take(take).collect();
        self.anim_buffer = self.anim_buffer.chars().skip(take).collect();
        self.anim_last = std::time::Instant::now();
        self.append_streaming(&reveal, MsgKind::Reasoning);
    }

    /// Vuelca el buffer de animación al instante (Espacio / nuevo prompt).
    fn flush_anim(&mut self) {
        if !self.anim_buffer.is_empty() {
            let rest = std::mem::take(&mut self.anim_buffer);
            self.append_streaming(&rest, MsgKind::Reasoning);
        }
    }

    /// Mensaje de ayuda si el modelo tarda demasiado (upstream saturado).
    fn maybe_streaming_hint(&mut self) {
        // ¿ya llegó texto del asistente en ESTE turno (desde el último prompt)?
        let has_text = self
            .messages
            .iter()
            .rev()
            .take_while(|m| m.role != Role::User)
            .any(|m| m.role == Role::Assistant);
        if let Some(start) = self.streaming_since {
            if !self.hint_shown && !has_text && start.elapsed() > std::time::Duration::from_secs(20) {
                self.hint_shown = true;
                self.push(
                    Role::System,
                    MsgKind::Text,
                    "sin respuesta tras 20s — el proveedor del modelo puede estar \
                     sobrecargado (Z.ai a veces devuelve 529/429); el turno sigue reintentando",
                );
            }
        }
    }

    fn push(&mut self, role: Role, kind: MsgKind, content: impl Into<String>) {
        self.messages.push(Message {
            role,
            kind,
            content: cap_content(&content.into()),
            expanded: true,
        });
    }

    /// Colapsa todos los tool calls y diffs: al cerrar el turno solo quedan
    /// sus líneas compactas `▸ …`, como en opencode.
    fn collapse_tools(&mut self) {
        for m in &mut self.messages {
            if matches!(m.role, Role::Tool | Role::Diff) {
                m.expanded = false;
            }
        }
    }

    /// `o`: si hay algo colapsado lo expande todo; si no, colapsa todo.
    fn toggle_tools(&mut self) {
        let any_collapsed = self
            .messages
            .iter()
            .any(|m| matches!(m.role, Role::Tool | Role::Diff) && !m.expanded);
        for m in &mut self.messages {
            if matches!(m.role, Role::Tool | Role::Diff) {
                m.expanded = any_collapsed;
            }
        }
    }

    /// Texto del prompt listo para enviar (líneas unidas).
    fn input_text(&self) -> String {
        self.input.join("\n")
    }

    /// Ítems a mostrar en el modal abierto (ya con formato de display).
    pub fn modal_items(&self) -> Vec<String> {
        match &self.modal {
            Some(Modal::Commands { .. }) => COMMANDS
                .iter()
                .map(|c| format!("{}  —  {}", c.name, c.hint))
                .collect(),
            Some(Modal::Files { .. }) => self.files.clone(),
            Some(Modal::Skills { .. }) => self
                .skills
                .iter()
                .map(|s| {
                    if s.description.is_empty() {
                        s.name.clone()
                    } else {
                        format!("{}  —  {}", s.name, s.description)
                    }
                })
                .collect(),
            None => Vec::new(),
        }
    }

    /// Índices filtrados por la query del modal (fuzzy, ordenados por puntaje).
    pub fn modal_filtered(&self) -> Vec<usize> {
        let query = match &self.modal {
            Some(Modal::Commands { query, .. })
            | Some(Modal::Files { query, .. })
            | Some(Modal::Skills { query, .. }) => query.as_str(),
            None => "",
        };
        let items = self.modal_items();
        crate::modal::filter_indices(query, &items)
    }

    fn modal_query_mut(&mut self) -> Option<&mut String> {
        match &mut self.modal {
            Some(Modal::Commands { query, .. })
            | Some(Modal::Files { query, .. })
            | Some(Modal::Skills { query, .. }) => Some(query),
            None => None,
        }
    }

    /// Enter en el modal: ejecutar comando / insertar path / insertar skill.
    fn modal_pick(&mut self) {
        let filtered = self.modal_filtered();
        let sel = match &self.modal {
            Some(Modal::Commands { sel, .. })
            | Some(Modal::Files { sel, .. })
            | Some(Modal::Skills { sel, .. }) => *sel,
            None => return,
        };
        let items = self.modal_items();
        let Some(&idx) = filtered.get(sel) else { return };
        let chosen = items[idx].clone();
        match &self.modal {
            Some(Modal::Commands { .. }) => {
                let name = chosen.split("  —  ").next().unwrap_or("").to_string();
                self.modal = None;
                self.run_command(&name);
            }
            Some(Modal::Files { .. }) => {
                // insertar el path relativo en el cursor del prompt
                let path = chosen.clone();
                self.modal = None;
                self.insert_input(&path);
                self.insert_input(" ");
            }
            Some(Modal::Skills { .. }) => {
                let name = chosen.split("  —  ").next().unwrap_or("").to_string();
                self.modal = None;
                self.insert_input(&format!("${name} "));
            }
            None => {}
        }
    }

    fn open_files_modal(&mut self) {
        if self.files.is_empty() {
            self.files = crate::modal::discover_files(&self.workspace);
        }
        self.modal = Some(Modal::Files { sel: 0, query: String::new() });
    }

    fn open_skills_modal(&mut self) {
        if self.skills.is_empty() {
            self.skills = crate::modal::discover_skills(&self.workspace);
        }
        self.modal = Some(Modal::Skills { sel: 0, query: String::new() });
    }

    /// Ejecuta un comando de la paleta `/`.
    fn run_command(&mut self, name: &str) {
        match name {
            "modelo" => {
                if !self.models.is_empty() {
                    self.picker = Some(Picker::Models(0));
                }
            }
            "modo" => self.picker = Some(Picker::Mode(0)),
            "fork" => {
                if self.streaming {
                    self.push(
                        Role::System,
                        MsgKind::Text,
                        "no puedes bifurcar con un turno corriendo — esc lo detiene primero",
                    );
                } else {
                    self.connector.fork_session();
                    self.push(Role::System, MsgKind::Text, "bifurcando la sesión…");
                }
            }
            "compact" => {
                self.connector.compact();
                self.push(Role::System, MsgKind::Text, "compactando el contexto de la sesión…");
            }
            "sesiones" => {
                if !self.sessions.is_empty() {
                    self.sidebar = Some(0);
                }
            }
            "tema" => {
                self.theme = crate::theme::next(self.theme);
                crate::theme::save(self.theme);
                self.push(Role::System, MsgKind::Text, format!("tema → {}", self.theme.name));
            }
            "tools" => self.toggle_tools(),
            "archivos" => {
                if self.input_text().trim().is_empty() {
                    self.insert_input("@");
                }
                self.open_files_modal();
            }
            "limpiar" => {
                self.messages.clear();
                self.tool_msg_index.clear();
                self.anim_buffer.clear();
                self.scroll = 0;
                self.auto_scroll = true;
                self.push(Role::System, MsgKind::Text, "transcript limpiado");
            }
            "salir" => self.should_quit = true,
            _ => {}
        }
    }

    fn clear_input(&mut self) {
        self.input = vec![String::new()];
        self.input_cursor = InputCursor::default();
    }

    /// Inserta texto (tecla o paste) respetando \n y el cursor.
    fn insert_input(&mut self, text: &str) {
        for (i, part) in text.split('\n').enumerate() {
            if i > 0 {
                self.split_line_at_cursor();
            }
            let row = self.input_cursor.row;
            let col = self.input_cursor.col;
            if let Some(line) = self.input.get_mut(row) {
                let byte = Self::char_to_byte(line, col);
                line.insert_str(byte, part);
                self.input_cursor.col += part.chars().count();
            }
        }
    }

    fn split_line_at_cursor(&mut self) {
        let row = self.input_cursor.row;
        let col = self.input_cursor.col;
        let rest = {
            let line = &mut self.input[row];
            let byte = Self::char_to_byte(line, col);
            line.split_off(byte)
        };
        self.input.insert(row + 1, rest);
        self.input_cursor.row += 1;
        self.input_cursor.col = 0;
    }

    fn char_to_byte(s: &str, char_idx: usize) -> usize {
        s.char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(s.len())
    }

    /// `extra_rx`: stream de frames del agente real (modo zcode); se bombea al
    /// canal interno de eventos.
    pub async fn run(
        &mut self,
        mut extra_rx: Option<mpsc::UnboundedReceiver<AgentEvent>>,
    ) -> anyhow::Result<()> {
        let (tx, mut rx) = mpsc::unbounded_channel::<AgentEvent>();
        if let Some(mut extra) = extra_rx.take() {
            let pump_tx = tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = extra.recv().await {
                    if pump_tx.send(ev).is_err() {
                        break;
                    }
                }
            });
        }
        self.event_tx = Some(tx);

        // UN solo lector de teclado permanente: las teclas van por canal y no
        // se pierden aunque el bucle siga redibujando.
        let (key_tx, mut key_rx) = mpsc::unbounded_channel::<crossterm::event::Event>();
        std::thread::spawn(move || {
            loop {
                match crossterm::event::read() {
                    Ok(ev) => {
                        if key_tx.send(ev).is_err() {
                            break; // la TUI cerró
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let mut terminal = ratatui::init();
        // sin captura de mouse por defecto: la selección/copiar del terminal
        // funciona siempre; `m` activa la rueda del scroll cuando se quiera
        // bracketed paste: pegar código multi-línea no lo envía
        {
            use crossterm::execute;
            let _ = execute!(
                terminal.backend_mut(),
                crossterm::event::EnableBracketedPaste
            );
        }
        let res = self.event_loop(&mut terminal, &mut rx, &mut key_rx).await;
        if self.mouse_capture {
            use crossterm::execute;
            let _ = execute!(
                terminal.backend_mut(),
                crossterm::event::DisableMouseCapture
            );
        }
        {
            use crossterm::execute;
            let _ = execute!(
                terminal.backend_mut(),
                crossterm::event::DisableBracketedPaste
            );
        }
        ratatui::restore();
        res
    }

    async fn event_loop(
        &mut self,
        terminal: &mut ratatui::DefaultTerminal,
        rx: &mut mpsc::UnboundedReceiver<AgentEvent>,
        key_rx: &mut mpsc::UnboundedReceiver<crossterm::event::Event>,
    ) -> anyhow::Result<()> {
        loop {
            terminal.draw(|f| ui::draw(f, self))?;

            self.tick = self.tick.wrapping_add(1);
            self.drain_anim_buffer();
            self.maybe_streaming_hint();
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                    continue; // redibujo periódico (typewriter + spinner + cronómetro)
                }
                maybe_ev = rx.recv() => {
                    match maybe_ev {
                        Some(AgentEvent::Delta(d)) => {
                            // la respuesta llega: el thinking pendiente se completa primero
                            self.flush_anim();
                            self.append_streaming(&d, MsgKind::Text)
                        }
                        Some(AgentEvent::ReasoningDelta(d)) => {
                            if self.streaming {
                                // typewriter: el thinking se revela gradualmente;
                                // el reloj arranca con el primer carácter (si no,
                                // el tiempo acumulado volcaría todo de golpe)
                                if self.anim_buffer.is_empty() {
                                    self.anim_last = std::time::Instant::now();
                                }
                                self.anim_buffer.push_str(&d);
                            } else {
                                // fuera de un turno (historial del resume): al instante
                                self.append_streaming(&d, MsgKind::Reasoning);
                            }
                        }
                        Some(AgentEvent::ToolCall { call_id, line }) => {
                            self.activity_since_prompt = true;
                            if let Some(&idx) = self.tool_msg_index.get(&call_id) {
                                if let Some(m) = self.messages.get_mut(idx) {
                                    m.content = line;
                                }
                            } else {
                                self.push(Role::Tool, MsgKind::Text, line);
                                self.tool_msg_index.insert(call_id, self.messages.len() - 1);
                            }
                        }
                        Some(AgentEvent::ToolDiff { path, additions, deletions, lines }) => {
                            let mut content = format!("─ {}  +{} −{}", path, additions, deletions);
                            for l in lines {
                                content.push('\n');
                                content.push_str(&l);
                            }
                            self.push(Role::Diff, MsgKind::Text, content)
                        }
                        Some(AgentEvent::ToolOutput(out)) => {
                            let indented = out
                                .lines()
                                .map(|l| format!("  │ {l}"))
                                .collect::<Vec<_>>()
                                .join("\n");
                            self.push(Role::Tool, MsgKind::Text, indented)
                        }
                        Some(AgentEvent::PermissionRequest { token, tool_name, summary, options }) => {
                            self.activity_since_prompt = true;
                            let opts = options.iter().map(|(id, l)| format!("{id}:{l}")).collect::<Vec<_>>().join(", ");
                            self.push(
                                Role::System,
                                MsgKind::Text,
                                format!("⚠ PERMISO {tool_name}: {summary} — presiona y/n {opts}"),
                            );
                            self.pending_permission = Some((token, summary));
                        }
                        Some(AgentEvent::Done) => {
                            self.streaming = false;
                            self.streaming_since = None;
                            self.collapse_tools();
                            // drenar la cola de prompts: el siguiente sale solo
                            if !self.queue.is_empty() {
                                let next = self.queue.remove(0);
                                self.push(Role::User, MsgKind::Text, next.clone());
                                self.streaming = true;
                                self.streaming_since = Some(std::time::Instant::now());
                                self.hint_shown = false;
                                self.tool_msg_index.clear();
                                self.auto_scroll = true;
                                if let Some(tx) = self.event_tx.clone() {
                                    self.connector.send(next, tx);
                                }
                            } else {
                                self.connector.request_usage();
                            }
                        }
                        Some(AgentEvent::Forked { from: _, to }) => {
                            // cambiar a la copia: transcript limpio, historial nuevo
                            self.messages.clear();
                            self.tool_msg_index.clear();
                            self.anim_buffer.clear();
                            self.scroll = 0;
                            self.auto_scroll = true;
                            self.current_session = Some(to.clone());
                            self.push(
                                Role::System,
                                MsgKind::Text,
                                format!("sesión bifurcada → {} (la original quedó intacta)", &to[..to.len().min(8)]),
                            );
                        }
                        Some(AgentEvent::Usage(u)) => {
                            self.usage_text = Some(u);
                        }
                        Some(AgentEvent::UserInputRequest { token, prompt, questions }) => {
                            let summary = questions
                                .first()
                                .map(|q| q.question.clone())
                                .unwrap_or(prompt);
                            let n_opts = questions.first().map(|q| q.options.len()).unwrap_or(0);
                            self.push(
                                Role::System,
                                MsgKind::Text,
                                format!("❓ {summary} — elige 1-{n_opts} (esc cancela)"),
                            );
                            self.pending_user_input = Some(UserInputPending {
                                token,
                                questions,
                                question_idx: 0,
                                option_idx: 0,
                            });
                        }
                        Some(AgentEvent::Error(e)) => {
                            self.push(Role::System, MsgKind::Text, format!("error: {e}"));
                            self.streaming = false;
                            self.streaming_since = None;
                        }
                        Some(AgentEvent::ModelsAvailable(models)) => {
                            self.models = models;
                        }
                        Some(AgentEvent::SessionsList(list)) => {
                            self.sessions = list;
                        }
                        Some(AgentEvent::SessionReady(sid)) => {
                            self.current_session = Some(sid);
                        }
                        Some(AgentEvent::HistoryUser(text)) => {
                            // el prompt recién enviado ya está en el transcript;
                            // los frames v4 lo re-entregan → no duplicar
                            let dup = matches!(self.messages.last(),
                                Some(m) if m.role == Role::User && m.content == text);
                            if !dup {
                                self.push(Role::User, MsgKind::Text, text)
                            }
                        }
                        None => {}
                    }
                }
                maybe_key = key_rx.recv() => {
                    match maybe_key {
                        Some(crossterm::event::Event::Mouse(m)) => self.handle_mouse(m),
                        Some(crossterm::event::Event::Paste(text)) => {
                            if self.focus_input {
                                self.insert_input(&text);
                            }
                        }
                        Some(ev) => self.handle_key(ev),
                        None => {}
                    }
                }
            }
            if self.should_quit {
                return Ok(());
            }
        }
    }

    fn append_streaming(&mut self, delta: &str, kind: MsgKind) {
        // guard anti-duplicado: los frames v4 re-entregan contenido por
        // initial/online/recovery en CUALQUIER orden (el thinking puede llegar
        // de nuevo después del texto); un bloque grande ya presente en otro
        // mensaje del mismo tipo es re-entrega, no texto nuevo
        if delta.chars().count() >= 24
            && self
                .messages
                .iter()
                .any(|m| m.role == Role::Assistant && m.kind == kind && m.content.contains(delta))
        {
            return;
        }
        if let Some(last) = self.messages.last_mut() {
            if last.role == Role::Assistant && last.kind == kind {
                last.content.push_str(delta);
                last.content = cap_content(&last.content);
                return;
            }
        }
        self.push(Role::Assistant, kind, delta);
    }

    fn handle_key(&mut self, ev: crossterm::event::Event) {
        use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
        let Event::Key(key) = ev else { return };
        if key.kind != KeyEventKind::Press {
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // Ctrl+S: abrir/cerrar la sidebar de sesiones
        if ctrl && key.code == KeyCode::Char('s') {
            self.sidebar = if self.sidebar.is_some() {
                None
            } else if !self.sessions.is_empty() {
                Some(0)
            } else {
                None
            };
            return;
        }
        // Ctrl+T: ciclar tema
        if ctrl && key.code == KeyCode::Char('t') {
            self.theme = crate::theme::next(self.theme);
            crate::theme::save(self.theme);
            self.push(Role::System, MsgKind::Text, format!("tema → {}", self.theme.name));
            return;
        }
        // Ctrl+O: picker de modo (build/edit/plan/yolo) — Ctrl+M es Enter en terminals
        if ctrl && key.code == KeyCode::Char('o') {
            self.picker = if self.picker.is_some() {
                None
            } else {
                Some(Picker::Mode(0))
            };
            return;
        }
        // renombrado de sesión en curso (sidebar + r): captura el teclado
        if let Some(buf) = self.rename.clone() {
            match key.code {
                KeyCode::Enter => {
                    let title = buf.trim().to_string();
                    self.rename = None;
                    if !title.is_empty() {
                        if let Some(idx) = self.sidebar {
                            if let Some(si) = self.sessions.get_mut(idx) {
                                si.title = title.clone();
                            }
                        }
                        self.connector.rename_session(title);
                        self.push(Role::System, MsgKind::Text, "sesión renombrada");
                    }
                }
                KeyCode::Esc => self.rename = None,
                KeyCode::Backspace => {
                    let mut b = buf;
                    b.pop();
                    self.rename = Some(b);
                }
                KeyCode::Char(c) if !ctrl && !alt => {
                    self.rename = Some(format!("{buf}{c}"));
                }
                _ => {}
            }
            return;
        }
        // navegación de la sidebar
        if let Some(idx) = self.sidebar {
            match key.code {
                KeyCode::Up => {
                    self.sidebar = Some(idx.saturating_sub(1));
                    return;
                }
                KeyCode::Down => {
                    self.sidebar = Some((idx + 1).min(self.sessions.len().saturating_sub(1)));
                    return;
                }
                KeyCode::Enter => {
                    if let Some(si) = self.sessions.get(idx).cloned() {
                        // limpiar el chat: el historial de la sesión reemplaza
                        // la conversación actual, no se acumula
                        self.messages.clear();
                        self.tool_msg_index.clear();
                        self.anim_buffer.clear();
                        self.scroll = 0;
                        self.auto_scroll = true;
                        self.streaming = false;
                        self.streaming_since = None;
                        self.push(
                            Role::System,
                            MsgKind::Text,
                            format!("reanudando sesión: {}", si.title),
                        );
                        self.connector.resume_session(si.session_id);
                    }
                    self.sidebar = None;
                    return;
                }
                KeyCode::Esc => {
                    self.sidebar = None;
                    self.delete_armed = false;
                    return;
                }
                // r renombra la sesión seleccionada
                KeyCode::Char('r') => {
                    if self.sessions.get(idx).is_some() {
                        let current = self
                            .sessions
                            .get(idx)
                            .map(|si| si.title.clone())
                            .unwrap_or_default();
                        self.rename = Some(current);
                    }
                    return;
                }
                // d dos veces borra la sesión (la segunda confirma)
                KeyCode::Char('d') => {
                    if self.delete_armed {
                        self.delete_armed = false;
                        if let Some(si) = self.sessions.get(idx).cloned() {
                            if self.current_session.as_deref() == Some(si.session_id.as_str()) {
                                self.connector.delete_session();
                                self.messages.clear();
                                self.tool_msg_index.clear();
                                self.current_session = None;
                            }
                            self.sessions.remove(idx);
                            if self.sessions.is_empty() {
                                self.sidebar = None;
                            } else {
                                self.sidebar = Some(idx.min(self.sessions.len() - 1));
                            }
                            self.push(Role::System, MsgKind::Text, "sesión borrada");
                        }
                    } else {
                        self.delete_armed = true;
                        self.push(Role::System, MsgKind::Text, "d de nuevo confirma el borrado");
                    }
                    return;
                }
                _ => {}
            }
        }
        // Ctrl+P: abrir/cerrar el picker de modelos
        if ctrl && key.code == KeyCode::Char('p') {
            self.picker = if self.picker.is_some() {
                None
            } else if !self.models.is_empty() {
                Some(Picker::Models(0))
            } else {
                None
            };
            return;
        }
        // navegación del picker (dos etapas: modelo → nivel)
        if let Some(picker) = &self.picker {
            match picker {
                Picker::Models(idx) => {
                    match key.code {
                        KeyCode::Up => {
                            self.picker = Some(Picker::Models(idx.saturating_sub(1)));
                        }
                        KeyCode::Down => {
                            self.picker = Some(Picker::Models(
                                (*idx + 1).min(self.models.len().saturating_sub(1)),
                            ));
                        }
                        KeyCode::Enter => {
                            if let Some(m) = self.models.get(*idx).cloned() {
                                self.picker = Some(Picker::Levels(m, 0));
                                return;
                            }
                            self.picker = None;
                        }
                        KeyCode::Esc => self.picker = None,
                        _ => {}
                    }
                    return;
                }
                Picker::Levels(model, idx) => {
                    match key.code {
                        KeyCode::Up => {
                            let n = self.reasoning_levels.len();
                            self.picker = Some(Picker::Levels(
                                model.clone(),
                                (*idx + n - 1) % n,
                            ));
                        }
                        KeyCode::Down => {
                            let n = self.reasoning_levels.len();
                            self.picker = Some(Picker::Levels(
                                model.clone(),
                                (*idx + 1) % n,
                            ));
                        }
                        KeyCode::Enter => {
                            let level = self
                                .reasoning_levels
                                .get(*idx)
                                .cloned()
                                .unwrap_or_else(|| "max".into());
                            self.connector.set_model(
                                model.provider_id.clone(),
                                model.model_id.clone(),
                                Some(level.clone()),
                            );
                            self.push(
                                Role::System,
                                MsgKind::Text,
                                format!(
                                    "modelo → {} ({}, nivel {level})",
                                    model.label, model.provider_label
                                ),
                            );
                            self.picker = None;
                        }
                        KeyCode::Esc => self.picker = None,
                        _ => {}
                    }
                    return;
                }
                Picker::Mode(idx) => {
                    const MODES: [&str; 4] = ["build", "edit", "plan", "yolo"];
                    match key.code {
                        KeyCode::Up => {
                            self.picker = Some(Picker::Mode(idx.saturating_sub(1)));
                        }
                        KeyCode::Down => {
                            self.picker = Some(Picker::Mode((*idx + 1).min(3)));
                        }
                        KeyCode::Enter => {
                            let mode = MODES.get(*idx).copied().unwrap_or("build").to_string();
                            self.connector.set_mode(mode.clone());
                            self.mode = mode.clone();
                            self.push(Role::System, MsgKind::Text, format!("modo → {mode}"));
                            self.picker = None;
                        }
                        KeyCode::Esc => self.picker = None,
                        _ => {}
                    }
                    return;
                }
            }
        }
        if self.modal.is_some() {
            match key.code {
                KeyCode::Up => {
                    if let Some(m) = &mut self.modal {
                        let sel = match m {
                            Modal::Commands { sel, .. }
                            | Modal::Files { sel, .. }
                            | Modal::Skills { sel, .. } => sel,
                        };
                        *sel = sel.saturating_sub(1);
                    }
                }
                KeyCode::Down => {
                    let n = self.modal_filtered().len();
                    if n > 0 {
                        if let Some(m) = &mut self.modal {
                            let sel = match m {
                                Modal::Commands { sel, .. }
                                | Modal::Files { sel, .. }
                                | Modal::Skills { sel, .. } => sel,
                            };
                            *sel = (*sel + 1).min(n - 1);
                        }
                    }
                }
                KeyCode::Enter => self.modal_pick(),
                KeyCode::Esc => self.modal = None,
                KeyCode::Backspace => {
                    if let Some(q) = self.modal_query_mut() {
                        q.pop();
                    }
                }
                KeyCode::Char(c) if !ctrl && !alt => {
                    if let Some(q) = self.modal_query_mut() {
                        q.push(c);
                    }
                }
                _ => {}
            }
            return;
        }
        // `/` con el prompt vacío abre la paleta de comandos
        if key.code == KeyCode::Char('/')
            && !ctrl
            && !alt
            && self.focus_input
            && self.input_text().trim().is_empty()
        {
            self.modal = Some(Modal::Commands { sel: 0, query: String::new() });
            return;
        }
        // `$` con el prompt vacío abre el modal de skills
        if key.code == KeyCode::Char('$')
            && !ctrl
            && !alt
            && self.focus_input
            && self.input_text().trim().is_empty()
        {
            self.open_skills_modal();
            return;
        }
        // o expande/colapsa los tool calls y diffs (fuera del input)
        if key.code == KeyCode::Char('o') && !self.focus_input {
            self.toggle_tools();
            return;
        }
        // c copia la última respuesta del asistente (OSC52)
        if key.code == KeyCode::Char('c') && !self.focus_input && !ctrl {
            if let Some(last) = self
                .messages
                .iter()
                .rev()
                .find(|m| m.role == Role::Assistant && m.kind == MsgKind::Text)
            {
                copy_to_clipboard(&last.content);
                self.push(Role::System, MsgKind::Text, "respuesta copiada al portapapeles");
            }
            return;
        }
        // e edita el último prompt: lo carga en el input para modificarlo y reenviar
        if key.code == KeyCode::Char('e') && !self.focus_input && !self.streaming {
            if let Some(last) = self.messages.iter().rev().find(|m| m.role == Role::User) {
                self.input = last.content.lines().map(|l| l.to_string()).collect();
                if self.input.is_empty() {
                    self.input = vec![String::new()];
                }
                self.input_cursor = InputCursor {
                    row: self.input.len() - 1,
                    col: self.input.last().map(|l| l.chars().count()).unwrap_or(0),
                };
                self.focus_input = true;
            }
            return;
        }
        // m alterna la rueda del mouse (y con ella, la selección de texto)
        if key.code == KeyCode::Char('m') && !self.focus_input {
            self.mouse_capture = !self.mouse_capture;
            use crossterm::execute;
            if self.mouse_capture {
                let _ = execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
            } else {
                let _ = execute!(std::io::stdout(), crossterm::event::DisableMouseCapture);
            }
            return;
        }
        // Espacio salta el typewriter del thinking
        if key.code == KeyCode::Char(' ') && !self.anim_buffer.is_empty() {
            self.flush_anim();
            return;
        }
        // permiso pendiente: y/n/a responden directamente
        if let Some((token, _)) = self.pending_permission {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.pending_permission = None;
                    self.connector.answer_permission(token, true, false);
                    return;
                }
                // always allow: regla permanente para esta tool
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    self.pending_permission = None;
                    self.connector.answer_permission(token, true, true);
                    self.push(Role::System, MsgKind::Text, "permitido siempre para esta tool");
                    return;
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.pending_permission = None;
                    self.connector.answer_permission(token, false, false);
                    return;
                }
                KeyCode::Esc => {
                    self.pending_permission = None;
                    self.connector.answer_permission(token, false, false);
                    return;
                }
                _ => {}
            }
        }
        // AskUserQuestion pendiente: 1-9 elige, ↑↓ cambia de pregunta, esc cancela
        if self.pending_user_input.is_some() {
            let n_questions = self
                .pending_user_input
                .as_ref()
                .map(|ui| ui.questions.len())
                .unwrap_or(0);
            let mut accept = false;
            let mut cancel = false;
            match key.code {
                KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                    if let Some(ui) = &mut self.pending_user_input {
                        let idx = c.to_digit(10).unwrap_or(1) as usize - 1;
                        if let Some(q) = ui.questions.get(ui.question_idx) {
                            if idx < q.options.len() {
                                ui.option_idx = idx;
                                accept = true;
                            }
                        }
                    }
                }
                KeyCode::Up => {
                    if let Some(ui) = &mut self.pending_user_input {
                        ui.question_idx = ui.question_idx.saturating_sub(1);
                        ui.option_idx = 0;
                    }
                }
                KeyCode::Down => {
                    if let Some(ui) = &mut self.pending_user_input {
                        if ui.question_idx + 1 < n_questions {
                            ui.question_idx += 1;
                            ui.option_idx = 0;
                        }
                    }
                }
                KeyCode::Enter => accept = true,
                KeyCode::Esc => cancel = true,
                _ => {}
            }
            if accept || cancel {
                if let Some(ui) = self.pending_user_input.take() {
                    let answers: Vec<(String, String)> = ui
                        .questions
                        .iter()
                        .enumerate()
                        .map(|(qi, q)| {
                            let oi = if qi == ui.question_idx {
                                ui.option_idx
                            } else {
                                0
                            };
                            let value = q
                                .options
                                .get(oi)
                                .map(|(v, _)| v.clone())
                                .unwrap_or_default();
                            (q.header.clone(), value)
                        })
                        .collect();
                    self.connector
                        .answer_user_input(ui.token, accept && !cancel, answers);
                }
            }
            return;
        }
        // PgUp/PgDown hacen scroll siempre, sin importar el foco
        if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            self.handle_scroll_key(key.code);
            return;
        }
        match key.code {
            KeyCode::Char('c') if ctrl => {
                self.should_quit = true;
            }
            _ if !self.focus_input => self.handle_scroll_key(key.code),
            // Alt+Enter: nueva línea en el prompt
            KeyCode::Enter if alt => self.split_line_at_cursor(),
            KeyCode::Enter => self.submit(),
            // `@` en el prompt: insertarlo y abrir el file picker
            KeyCode::Char('@') if !ctrl && !alt && self.focus_input => {
                self.insert_input("@");
                self.open_files_modal();
            }
            KeyCode::Char(c) if !ctrl && !alt => self.insert_input(&c.to_string()),
            KeyCode::Backspace => {
                let row = self.input_cursor.row;
                let col = self.input_cursor.col;
                if col > 0 {
                    if let Some(line) = self.input.get_mut(row) {
                        let byte = Self::char_to_byte(line, col);
                        // quitar 1 char antes del cursor
                        let prev = line[..byte]
                            .char_indices()
                            .next_back()
                            .map(|(b, _)| b)
                            .unwrap_or(0);
                        line.replace_range(prev..byte, "");
                        self.input_cursor.col -= 1;
                    }
                } else if row > 0 {
                    // unir con la línea anterior
                    let prev_len = self.input[row - 1].chars().count();
                    let cur = self.input.remove(row);
                    self.input[row - 1].push_str(&cur);
                    self.input_cursor.row -= 1;
                    self.input_cursor.col = prev_len;
                }
            }
            KeyCode::Left => {
                self.input_cursor.col = self.input_cursor.col.saturating_sub(1);
            }
            KeyCode::Right => {
                let row_len = self
                    .input
                    .get(self.input_cursor.row)
                    .map(|l| l.chars().count())
                    .unwrap_or(0);
                if self.input_cursor.col < row_len {
                    self.input_cursor.col += 1;
                }
            }
            KeyCode::Up => {
                if self.input_cursor.row > 0 {
                    self.input_cursor.row -= 1;
                    self.clamp_cursor_col();
                } else {
                    self.auto_scroll = false;
                    self.scroll = self.scroll.saturating_sub(1);
                }
            }
            KeyCode::Down => {
                if self.input_cursor.row + 1 < self.input.len() {
                    self.input_cursor.row += 1;
                    self.clamp_cursor_col();
                } else {
                    self.scroll = self.scroll.saturating_add(1);
                }
            }
            KeyCode::Home => self.input_cursor.col = 0,
            KeyCode::End => {
                self.input_cursor.col = self
                    .input
                    .get(self.input_cursor.row)
                    .map(|l| l.chars().count())
                    .unwrap_or(0);
            }
            // Esc interrumpe el turno si está trabajando; si no, limpia el input
            KeyCode::Esc if self.streaming => {
                self.connector.stop();
                self.push(Role::System, MsgKind::Text, "⏹ interrumpiendo turno…");
            }
            KeyCode::Esc => self.clear_input(),
            KeyCode::Tab => self.focus_input = !self.focus_input,
            _ => {}
        }
    }

    fn clamp_cursor_col(&mut self) {
        let row_len = self
            .input
            .get(self.input_cursor.row)
            .map(|l| l.chars().count())
            .unwrap_or(0);
        self.input_cursor.col = self.input_cursor.col.min(row_len);
    }

    fn handle_mouse(&mut self, m: crossterm::event::MouseEvent) {
        use crossterm::event::MouseEventKind;
        match m.kind {
            MouseEventKind::ScrollUp => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(3);
            }
            MouseEventKind::ScrollDown => {
                self.scroll = self.scroll.saturating_add(3);
            }
            _ => {}
        }
    }

    fn handle_scroll_key(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode;
        match code {
            KeyCode::Up => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(1);
            }
            KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(1);
            }
            KeyCode::PageUp => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(20);
            }
            KeyCode::PageDown => self.scroll = self.scroll.saturating_add(20),
            KeyCode::Esc => self.auto_scroll = true,
            KeyCode::Tab => self.focus_input = !self.focus_input,
            KeyCode::Char('q') => self.should_quit = true,
            _ => {}
        }
    }

    fn submit(&mut self) {
        let mut prompt = self.input_text().trim().to_string();
        if prompt.is_empty() {
            return;
        }
        // trabajando: el prompt se encola (estilo Desktop) y sale al terminar
        if self.streaming {
            self.queue.push(prompt);
            self.clear_input();
            self.push(
                Role::System,
                MsgKind::Text,
                format!("encolado ({} en cola) — saldrá al terminar el turno", self.queue.len()),
            );
            return;
        }
        // `$skill args` → prompt que le pide al agente cargar esa skill
        if let Some(body) = prompt.strip_prefix('$') {
            let (name, rest) = match body.split_once(' ') {
                Some((n, r)) => (n, r.trim()),
                None => (body, ""),
            };
            if !name.is_empty() {
                let path = self
                    .skills
                    .iter()
                    .find(|s| s.name == name)
                    .map(|s| s.path.display().to_string());
                let args = if rest.is_empty() {
                    "sin argumentos adicionales".to_string()
                } else {
                    format!("argumentos del usuario: {rest}")
                };
                prompt = match path {
                    Some(p) => format!(
                        "Usa la skill '{name}' — lee {p} y sigue sus instrucciones ({args})."
                    ),
                    None => format!(
                        "Usa la skill '{name}' de ZCode con el tool Skill y sigue sus instrucciones ({args})."
                    ),
                };
            }
        }
        self.flush_anim(); // la animación nunca bloquea un nuevo prompt
        self.clear_input();
        self.push(Role::User, MsgKind::Text, prompt.clone());
        self.streaming = true;
        self.streaming_since = Some(std::time::Instant::now());
        self.hint_shown = false;
        self.activity_since_prompt = false;
        self.tool_msg_index.clear();
        self.auto_scroll = true;

        if let Some(tx) = self.event_tx.clone() {
            self.connector.send(prompt, tx);
        }
    }
}

/// Tope de contenido por mensaje: sin esto, un thinking que cita un archivo
/// minificado de miles de líneas congela el render del transcript.
const MAX_MSG_LINES: usize = 400;
const MAX_MSG_CHARS: usize = 24_000;

fn cap_content(content: &str) -> String {
    let total_lines = content.lines().count();
    let total_chars = content.chars().count();
    if total_lines <= MAX_MSG_LINES && total_chars <= MAX_MSG_CHARS {
        return content.to_string();
    }
    let mut out = String::with_capacity(MAX_MSG_CHARS.min(total_chars) + 80);
    let mut lines = 0;
    for line in content.lines() {
        if lines >= MAX_MSG_LINES || out.chars().count() > MAX_MSG_CHARS {
            break;
        }
        out.push_str(line);
        out.push('\n');
        lines += 1;
    }
    out.push_str(&format!(
        "… [truncado: {total_lines} líneas / {total_chars} caracteres en el mensaje original]"
    ));
    out
}

/// Copia al portapapeles del terminal con OSC52 (sin dependencias).
fn copy_to_clipboard(text: &str) {
    use std::io::Write as _;
    let bytes = text.as_bytes();
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b64 = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        b64.push(TABLE[(b[0] >> 2) as usize] as char);
        b64.push(TABLE[(((b[0] & 0x03) << 4) | (b[1] >> 4)) as usize] as char);
        b64.push(if chunk.len() > 1 {
            TABLE[(((b[1] & 0x0f) << 2) | (b[2] >> 6)) as usize] as char
        } else {
            '='
        });
        b64.push(if chunk.len() > 2 { TABLE[(b[2] & 0x3f) as usize] as char } else { '=' });
    }
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b]52;c;{b64}\x07");
    let _ = out.flush();
}
