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

pub const COMMANDS: [Command; 7] = [
    Command { name: "modelo", hint: "elegir plan/modelo/nivel (ctrl+p)" },
    Command { name: "sesiones", hint: "listar y reanudar (ctrl+s)" },
    Command { name: "tema", hint: "ciclar tema (ctrl+t)" },
    Command { name: "tools", hint: "expandir/colapsar tools y diffs (o)" },
    Command { name: "archivos", hint: "buscar archivo del workspace (@)" },
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
            content: content.into(),
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
        if let Some(last) = self.messages.last_mut() {
            if last.role == Role::Assistant && last.kind == kind {
                // guard anti-duplicado: los frames v4 re-entregan texto por
                // initial/online/recovery; un bloque grande ya contenido en el
                // mensaje actual es re-entrega, no texto nuevo
                if delta.chars().count() >= 24 && last.content.contains(delta) {
                    return;
                }
                last.content.push_str(delta);
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
            }
        }
        // modal abierto: la captura de teclas es suya (búsqueda difusa)
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
        // permiso pendiente: y/n responden directamente
        if let Some((token, _)) = self.pending_permission {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.pending_permission = None;
                    if let Some(tx) = self.event_tx.clone() {
                        // reutilizamos cmd_tx vía Connector
                        self.connector.answer_permission(token, true);
                        let _ = tx;
                    }
                    return;
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.pending_permission = None;
                    self.connector.answer_permission(token, false);
                    return;
                }
                KeyCode::Esc => {
                    self.pending_permission = None;
                    self.connector.answer_permission(token, false);
                    return;
                }
                _ => {}
            }
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
        if prompt.is_empty() || self.streaming {
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
