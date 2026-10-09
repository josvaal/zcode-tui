# Roadmap de zcode-tui

Brechas contra ZCode Desktop, con el método del protocolo stdio que las habilita.
Lo tachado ya está implementado.

## Control del turno
- [x] Detener turno — `Esc` → `session/stop`
- [x] Cola de prompts — encola mientras trabaja, drena al terminar
- [x] Editar último prompt — `e` lo recarga en el input (re-envío como turno nuevo)
- [x] /compact — `session/compact`
- [x] Copiar respuesta — `c` con OSC52
- [ ] Steer (enviar YA interrumpiendo) — v4 `sendText` con `requestedDelivery: startNow`
- [ ] Editar in-place el turno (rewind del chat y archivos) — v4 `editUserQuery` + `applyFileRewind` (requieren CAS `baseRevision`/`baseLogEpoch`)
- [x] Fork de sesión — `/fork` → `session/fork` (latestCheckpoint, fallback por índice de turno)

## Sesión y contexto
- [x] Modo build/edit/plan/yolo — `Ctrl+O` → `session/setMode`
- [x] Uso de tokens en la status bar — `session/usage`
- [x] Renombrar/borrar sesión — sidebar `r` / `d`×2 → v4 `renameSession`/`deleteSession`
- [ ] Cuota del plan (ventana 5h, semanal) — `usage-stats` service (`getCodingPlanUsageSnapshot`)
- [ ] Medidor de ventana de contexto (desglose system/tools/mensajes)
- [ ] Grupos/pin de sesiones — `zcode-task` service

## Interacciones
- [x] AskUserQuestion con opciones numeradas — `interaction/requestUserInput`
- [x] Permisos "always allow" — `a` agrega `permissionUpdates.addRules`
- [ ] Adjuntos (imágenes/archivos) — `v4/attachment/begin|chunk|commit` (chunked base64)
- [ ] Subagentes visibles — `session/subagents`
- [ ] Elicitation MCP con texto libre

## Backlog mayor
- [ ] Terminal integrada (tab) — canal `terminal` (create/write/resize + stream)
- [ ] Git pane (status/diff/commit con mensaje IA) — canal `git`
- [ ] Checkpoints de archivos por turno — canal `git-checkpoint`
- [ ] Buscar en la conversación (Ctrl+F)
- [ ] Notificación de fin de turno (campana OSC)
- [ ] Plugins/MCP/skills: gestión — canales `plugins`, `mcp-sync`, `skills`
- [ ] Bots, automations, share de conversación, remoto/SSH
