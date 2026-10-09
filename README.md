# zcode-tui

Un cliente TUI en **Rust** para el harness de [ZCode](https://github.com/zai-org/ZCode):
chat con streaming real, agente con herramientas (archivos, bash), permisos interactivos,
diffs coloreados, sesiones persistidas y multi-proveedor — todo en la terminal.

## Captura mental

```
╭─ ✻ thinking
│ The user wants me to create a file...
╰─
zcode ◂
Listo, creé hola-mundo.cpp. Para compilarlo:

╭─ bash
│ g++ hola-mundo.cpp -o hola-mundo && ./hola-mundo
╰─

  ⚠ Write — esperando aprobación          ← y/n
  ✔ Write — {"file_path": "hola-mundo.cpp"}
  ─ hola-mundo.cpp  +8 −0                 ← diff coloreado
  + #include <iostream>
  + int main() { ... }
```

## Características

- **Arranque automático**: detecta el runtime de ZCode (CLI instalado o el bundle
  embebido de ZCode Desktop), lanza `app-server` en modo headless y se conecta.
- **Streaming real en vivo**: thinking (`✻`) y respuesta token a token por el canal
  `session/subscribe` (deliveryKind `desktop-continuous`), con typewriter opcional.
- **Multi-proveedor / multi-plan**: `Ctrl+P` para elegir plan, modelo y nivel de
  reasoning; se recuerda entre corridas (`~/.config/zcode-tui/model.json`).
- **Agente completo**: tool calls con estado en vivo, aprobaciones `y/n`, diffs
  coloreados (+verde/−rojo) y salida de bash.
- **Sesiones**: `Ctrl+S` lista las sesiones del workspace y reanuda con historial.
- **Errores visibles**: los fallos del provider (429/529/1310…) se muestran en el
  transcript con su código y mensaje completo.

## Requisitos

- El runtime de ZCode: [zai-org/ZCode](https://github.com/zai-org/ZCode)
  (CLI instalado en `~/.local/bin/zcode`, o ZCode Desktop instalado — su bundle
  embebido se detecta automáticamente).
- Node.js **≥ 24** (el CLI lo exige; se elige el más nuevo disponible).
- Rust estable para compilar.

## Compilar y usar

```bash
git clone https://github.com/josvaal/zcode-tui
cd zcode-tui
cargo build --release
./target/release/zcode-tui
```

Sin argumentos: busca el runtime, levanta el servidor y entra al chat.
La sesión se crea con tu configuración de providers de ZCode
(`~/.zcode/v2/provider_config.json` si existe).

### Flags

```
--url ws://host:puerto   conectar a un servidor ya corriendo (WebSocket)
--token TOKEN            token de autenticación del servidor
--workspace /ruta        workspace del agente (default: cwd)
--demo                   modo demostración sin runtime
--zcode-bin /ruta        ruta explícita al binario zcode
--model proveedor/modelo[@nivel]   ej: zai-api/GLM-5.3-Flash@high
```

### Atajos

| Tecla | Acción |
|---|---|
| `Enter` | enviar prompt |
| `Ctrl+P` | plan / modelo / nivel de reasoning |
| `Ctrl+S` | sesiones del workspace (reanudar) |
| `y` / `n` | aprobar / denegar permisos |
| `Espacio` | saltar animación del thinking |
| `PgUp/PgDn`, `tab`+`m`+rueda | scroll |
| `Ctrl+C` | salir |

## Arquitectura

```
crates/
├── zcode-client    protocolo binario RPC de ZCode (frames 13B, VQL, canales)
├── zcode-launcher  detección de runtime, Node ≥24, provider config
└── zcode-tui       la app (ratatui + crossterm)
```

La TUI habla el **ZCode Protocol** (NDJSON JSON-RPC por stdio) con `zcode app-server`:
`session/create` → `v4/conversation/subscribe` + `session/subscribe` → `session/send`,
consumiendo los frames v4 (tool calls, diffs, estados) y el stream legacy en vivo
(`reasoning_delta`/`text_delta`). El protocolo de referencia vive en
`packages/shared/src/zcode-protocol*` del monorepo de ZCode.

## Licencia

Apache-2.0 (compatible con ZCode).
