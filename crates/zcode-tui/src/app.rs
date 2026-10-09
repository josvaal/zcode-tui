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
}

pub struct App {
    pub theme: Theme,
    pub streaming_since: Option<std::time::Instant>,
    pub hint_shown: bool,
    pub messages: Vec<Message>,
    pub input: String,
    pub scroll: u16,
    pub auto_scroll: bool,
    pub focus_input: bool,
    pub streaming: bool,
    pub connector: Connector,
    pub user_label: String,
    pub should_quit: bool,
    pub pending_permission: Option<(u64, String)>, // (token, summary)
    pub mouse_capture: bool,
    pub models: Vec<ModelInfo>,
    /// picker abierto: lista de modelos o selector de nivel para uno elegido
    pub picker: Option<Picker>,
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
    pub fn new(connector: Connector) -> App {
        App {
            theme: crate::theme::DARK,
            streaming_since: None,
            hint_shown: false,
            messages: vec![Message {
                role: Role::System,
                kind: MsgKind::Text,
                content: "zcode-tui listo — escribe un prompt y presiona Enter".into(),
            }],
            input: String::new(),
            scroll: 0,
            auto_scroll: true,
            focus_input: true,
            streaming: false,
            connector,
            user_label: "tú".into(),
            should_quit: false,
            pending_permission: None,
            mouse_capture: false,
            tool_msg_index: Default::default(),
            activity_since_prompt: false,
            models: Vec::new(),
            picker: None,
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
        });
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
        let res = self.event_loop(&mut terminal, &mut rx, &mut key_rx).await;
        if self.mouse_capture {
            use crossterm::execute;
            let _ = execute!(
                terminal.backend_mut(),
                crossterm::event::DisableMouseCapture
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

            self.drain_anim_buffer();
            self.maybe_streaming_hint();
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {
                    continue; // redibujo periódico (typewriter + cronómetro)
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
                            self.push(Role::User, MsgKind::Text, text)
                        }
                        None => {}
                    }
                }
                maybe_key = key_rx.recv() => {
                    match maybe_key {
                        Some(crossterm::event::Event::Mouse(m)) => self.handle_mouse(m),
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
        // Ctrl+S: abrir/cerrar la sidebar de sesiones
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.sidebar = if self.sidebar.is_some() {
                None
            } else if !self.sessions.is_empty() {
                Some(0)
            } else {
                None
            };
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
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.code == KeyCode::Char('p')
        {
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
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            _ if !self.focus_input => self.handle_scroll_key(key.code),
            KeyCode::Enter => self.submit(),
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Esc => self.input.clear(),
            KeyCode::Tab => self.focus_input = !self.focus_input,
            _ => {}
        }
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
        let prompt = self.input.trim().to_string();
        if prompt.is_empty() || self.streaming {
            return;
        }
        self.flush_anim(); // la animación nunca bloquea un nuevo prompt
        self.input.clear();
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
