# Solução de problemas

[← Voltar ao README](../README.md)

- [O botão Guide não abre nada](#o-botão-guide-não-abre-nada)
- [O Guide abre a Steam em vez do Orbit](#o-guide-abre-a-steam-em-vez-do-orbit)
- [O botão X não fecha programas (ou é o Y que fecha)](#o-botão-x-não-fecha-programas-ou-é-o-y-que-fecha)
- [O Orbit não fica por cima do jogo](#o-orbit-não-fica-por-cima-do-jogo)
- [A aba Programas está vazia](#a-aba-programas-está-vazia)
- [Uma saída de áudio não aparece](#uma-saída-de-áudio-não-aparece)
- [Bluetooth: não liga / "bloqueado pelo sistema"](#bluetooth-não-liga--bloqueado-pelo-sistema)
- [Bluetooth: o pareamento falha](#bluetooth-o-pareamento-falha)
- [Gravar / Instant Replay não funcionam](#gravar--instant-replay-não-funcionam)
- [Ver os logs](#ver-os-logs)

---

## O botão Guide não abre nada

1. **O Orbit está rodando?**
   ```sh
   pgrep -a orbit
   ```
   Se não aparecer nada, rode `./install.sh` (ou `orbit --daemon`).

2. **O controle foi detectado?** Rode em primeiro plano e aperte o Guide:
   ```sh
   orbit --quit; orbit --daemon
   ```
   Deve aparecer `orbit: controle conectado: <nome do controle>`.

3. **Permissão no dispositivo.** O controle precisa ser legível pelo seu usuário:
   ```sh
   grep -A4 -i "controller\|gamepad\|joystick" /proc/bus/input/devices   # ache o eventN
   getfacl /dev/input/eventN
   ```
   Deve haver uma linha `user:<seu usuário>:rw-` (regra *uaccess* do systemd). Se não
   houver, adicione seu usuário ao grupo `input` (`sudo usermod -aG input $USER`) e
   entre de novo na sessão.

4. **O controle manda o botão Guide?** Alguns controles têm modos (Switch, D-input,
   X-input) e em alguns deles o botão Home não envia `BTN_MODE`. Teste com
   `evtest /dev/input/eventN` e, se preciso, troque o modo do controle.

## O Guide abre a Steam em vez do Orbit

Em *Steam → Configurações → Controle*, desative **"O botão Guia foca a Steam"**.

## O botão X não fecha programas (ou é o Y que fecha)

O Orbit escuta o código `BTN_X`/`BTN_NORTH` (0x133), que é o **X** em controles Xbox e
compatíveis usando o driver `xpad`. Outros drivers (ex.: PlayStation via
`hid-playstation`) numeram os botões de outro jeito. Você sempre pode usar o botão
**✕** ao lado de cada item, com a direita do direcional e **A**.

## O Orbit não fica por cima do jogo

O overlay usa *layer-shell*. Confira:

- o Orbit foi compilado com `gtk4-layer-shell` (o `install.sh` avisa quando não foi);
- o ambiente suporta layer-shell: COSMIC, KDE Plasma, Hyprland, Sway, niri… sim; GNOME
  e X11 não. Nesses o Orbit abre como janela comum em tela cheia.

Ao rodar `orbit --daemon` num terminal, a mensagem
`layer-shell indisponível, usando janela comum em tela cheia` indica o modo janela.

## A aba Programas está vazia

```sh
orbit --list-windows
```

- `backend: wayland (cosmic)`, `wayland` ou `x11` com janelas listadas: está funcionando.
- `nenhum backend de janelas disponível`: o ambiente não expõe as janelas (caso do
  GNOME no Wayland).

## Uma saída de áudio não aparece

```sh
orbit --list-audio
```

Mostra exatamente o que a aba vai listar (`*` = padrão, `-` = desconectada). Saídas
sem nada plugado ficam escondidas em **"▾ Mostrar saídas desconectadas"**. Se uma
porta não aparece nem ali, confira `pactl list cards`: o Orbit lista as portas que o
PipeWire/PulseAudio informa.

## Bluetooth: não liga / "bloqueado pelo sistema"

O botão **Bluetooth** tenta `rfkill unblock bluetooth` sozinho quando o adaptador está
bloqueado por software. Se continuar bloqueado:

```sh
rfkill list bluetooth
systemctl status bluetooth
```

- `Hard blocked: yes`: há uma chave física ou uma opção da BIOS desligando o Bluetooth.
- serviço parado: `sudo systemctl enable --now bluetooth`.

## Bluetooth: o pareamento falha

- Coloque o dispositivo em **modo de pareamento** antes de apertar A (controles: em
  geral segurar o botão de sync; fones: segurar o botão de ligar).
- "não respondeu": o dispositivo saiu do modo de pareamento ou está longe.
- Se ele já foi pareado com outro sistema neste PC (dual boot), esqueça-o (**X**) e
  pareie de novo.
- Se aparecer um código, confira se é o mesmo do dispositivo e escolha **Confirmar**.

## Gravar / Instant Replay não funcionam

Instale o [`gpu-screen-recorder`](https://git.dec05eba.com/gpu-screen-recorder/about/).
No Wayland ele pede permissão de captura de tela na primeira vez.

## Ver os logs

```sh
orbit --quit
orbit --daemon        # deixa os logs no terminal
```
