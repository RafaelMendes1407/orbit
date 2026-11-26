# Arquitetura e desenvolvimento

[← Voltar ao README](../README.md)

## Visão geral

O Orbit é um único binário Rust + GTK4 que fica **residente**: a janela é montada uma
vez na inicialização e depois só é mostrada ou escondida. Toda fonte de dados externa
(controle, janelas, Bluetooth, áudio) é lida por eventos ou fora da thread da interface,
então o painel abre na hora e o processo não gasta CPU parado.

```text
                    ┌──────────────────── thread da interface (GTK) ───────────────────┐
 /dev/input ──evdev──▶ gamepad.rs ──canal──▶ main.rs (Ui: abas, foco, navegação)        │
 (thread por controle)                         │                                        │
                                               ├─ games.rs     Steam + .desktop         │
 compositor ─Wayland/X11─▶ windows.rs ─canal──▶├─ windows.rs   programas abertos        │
 (thread própria)                              ├─ audio.rs ────pactl (spawn_blocking)   │
 BlueZ ───────D-Bus (sinais)─────────────────▶ ├─ bluetooth.rs D-Bus + agente           │
                                               ├─ capture.rs   prints, gpu-screen-recorder
                                               └─ system.rs    atalhos e energia        │
                    └───────────────────────────────────────────────────────────────────┘
```

## Módulos

| Arquivo | Responsabilidade |
|---|---|
| `src/main.rs` | Aplicação GTK, janela (layer-shell ou tela cheia), abas, navegação pelo controle, foco por aba, linha de comando |
| `src/gamepad.rs` | Descobre controles em `/dev/input`, uma thread bloqueante por controle, converte eventos em `PadEvent`, `EVIOCGRAB` com o painel aberto |
| `src/games.rs` | Lê `appmanifest_*.acf` e capas locais da Steam e os `.desktop` da categoria `Game` |
| `src/windows.rs` | Backends de janelas (COSMIC, wlr, KDE Plasma, X11/EWMH) e a aba Programas |
| `src/audio.rs` | Modelo de saídas/entradas a partir de `pactl -f json` (cards, sinks, sources), troca de perfil/porta/padrão |
| `src/bluetooth.rs` | BlueZ via D-Bus: adaptador, dispositivos, busca, pareamento e agente `org.bluez.Agent1` |
| `src/capture.rs` | Capturas (ferramenta nativa do ambiente ou portal XDG), gravação e Instant Replay |
| `src/system.rs` | Configurações, Arquivos (escolhidos pela área de trabalho), bloquear e energia |
| `src/util.rs` | Utilitários: rodar processos, notificações, pastas XDG, widgets comuns, detecção de ambiente |
| `src/style.css` | Tema |

## Controle e foco

- `gamepad.rs` reconhece um controle por ter `BTN_SOUTH`/`BTN_MODE` e o eixo `ABS_X`, e
  ignora o controle virtual da Steam para não duplicar eventos.
- O analógico usa histerese (engata em 60 %, solta abaixo de 35 %). O auto-repeat é
  feito na interface: 380 ms de atraso, depois a cada 110 ms.
- A navegação direcional acontece **só dentro da página visível**. Isso evita que, durante
  a animação de troca de aba, o foco caia na página que está saindo.
- Cada aba guarda uma referência fraca ao último widget focado. Se o foco se perder (o
  widget foi removido ou desabilitado), ele é recuperado na próxima volta do loop.
- Listas que mudam sozinhas (Programas, Áudio, Bluetooth) só são reconstruídas quando
  o conteúdo muda e devolvem o foco ao mesmo item, ou ao vizinho se ele sumiu.

## Programas abertos (`windows.rs`)

O backend é escolhido na inicialização, de acordo com o que o servidor gráfico oferece:

| Ordem | Protocolo | Ambientes | Fechar | Ativar |
|---|---|---|---|---|
| 1 | `ext_foreign_toplevel_list_v1` + `zcosmic_toplevel_info_v1` + `zcosmic_toplevel_manager_v1` | COSMIC | `manager.close` | `manager.activate` |
| 2 | `zwlr_foreign_toplevel_manager_v1` | Hyprland, Sway, niri, labwc, Wayfire, river | `handle.close` | `handle.activate(seat)` |
| 3 | `org_kde_plasma_window_management` | KDE Plasma | `window.close` | `set_state(active)` |
| 4 | `ext_foreign_toplevel_list_v1` sozinho | outros | — (só lista) | — |
| 5 | EWMH (`_NET_CLIENT_LIST`, `_NET_CLOSE_WINDOW`) | sessões X11 | `_NET_CLOSE_WINDOW` | `_NET_ACTIVE_WINDOW` |

A conexão Wayland (ou X11) é própria e roda numa thread que dorme no socket. Os proxies
Wayland são thread-safe, então a interface envia `close`/`activate` direto e faz `flush`.
"Fechar" é o mesmo pedido do botão X da janela: o programa pode perguntar se você quer salvar.

## Áudio (`audio.rs`)

A aba junta duas fontes:

1. **Nós existentes** (`pactl list sinks/sources`): viram o padrão com `set-default-*`.
2. **Portas das placas** que não estão ativas (`pactl list cards`): uma porta pode
   precisar de outro perfil (ex.: `output:hdmi-stereo-extra1` para o 2º monitor HDMI).
   O perfil escolhido prioriza: disponível, depois o que **mantém o outro lado** do perfil
   atual (não derrubar o microfone), depois a maior prioridade.

Ao escolher uma porta: `set-card-profile` (se preciso), espera o nó aparecer (até 3 s),
`set-*-port` e `set-default-*`. Portas com `availability: not available` aparecem
apenas em "desconectadas". Placas em `pro-audio` são cobertas pelos nós.

## Bluetooth (`bluetooth.rs`)

- Conexão com o barramento de sistema via `gio`. O estado vem de
  `ObjectManager.GetManagedObjects`, e as mudanças, dos sinais `InterfacesAdded`,
  `InterfacesRemoved` e `PropertiesChanged`, com a renderização agrupada em ~80 ms.
- `StartDiscovery` só com a aba aberta e o adaptador ligado. A busca para antes de um
  `Pair` (alguns adaptadores falham pareando enquanto procuram).
- Fluxo do **A**: não pareado → `Pair` → `Trusted=true` → `Connect`; pareado → `Connect`;
  conectado → `Disconnect`.
- O Orbit registra um agente `DisplayYesNo` em `/io/github/orbit/agent`, que **não** é o
  agente padrão do sistema. O BlueZ só o usa nos pareamentos iniciados pelo próprio Orbit.
  `RequestConfirmation` mostra o código com Confirmar/Cancelar; `DisplayPasskey` e
  `DisplayPinCode` mostram o código a digitar.
- Ligar com o adaptador bloqueado por software tenta `rfkill unblock bluetooth`.

## Compilação

```sh
cargo build --release                        # com layer-shell (padrão)
cargo build --release --no-default-features  # sem layer-shell (modo janela)
```

| Feature | Padrão | Efeito |
|---|---|---|
| `layer-shell` | sim | Overlay de verdade via `gtk4-layer-shell` (precisa da biblioteca do sistema) |

## Ferramentas de desenvolvimento

```sh
orbit --list-windows            # backend de janelas e o que ele enxerga
orbit --list-audio              # modelo da aba Áudio, com a ação de cada item
orbit --render-docs docs/screenshots   # regenera as capturas da documentação
```

O `--render-docs` roda uma instância separada (não conversa com a residente), abre o
painel em modo janela, espera os dados carregarem e renderiza o painel de cada aba em
PNG com `gtk::WidgetPaintable`. Isso pega só o painel, nunca o resto da tela.

## Ideias para o futuro

- Forçar o encerramento de um programa travado (ex.: segurar X).
- Mapeamento de botões configurável (controles PlayStation/Nintendo).
- Bateria dos dispositivos Bluetooth (`org.bluez.Battery1`).
