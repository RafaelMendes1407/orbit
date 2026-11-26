//! Leitura de controles via evdev, sem dependências de SDL.
//!
//! Uma thread "scanner" olha /dev/input a cada 2 s (só abre nós novos — custo ~zero)
//! e cria uma thread bloqueante por controle. As threads dormem no kernel até chegar
//! evento, então o consumo ocioso é nulo.

use evdev::{AbsoluteAxisCode, Device, EventSummary, KeyCode};
use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone, Copy)]
pub enum PadEvent {
    Guide,
    /// Direção pressionada (true) ou solta (false) — o app cuida do auto-repeat.
    Nav(Dir, bool),
    Accept,
    Back,
    PrevTab,
    NextTab,
    /// Botão X (no xpad chega como BTN_X = BTN_NORTH).
    Close,
}

/// _IOW('E', 0x90, int)
const EVIOCGRAB: u64 = 0x4004_4590;

/// "Agarra" os controles enquanto o overlay está aberto, para o jogo não receber
/// os comandos de navegação.
#[derive(Clone, Default)]
pub struct Grabber {
    fds: Arc<Mutex<HashMap<PathBuf, RawFd>>>,
    grabbed: Arc<AtomicBool>,
}

impl Grabber {
    pub fn set(&self, on: bool) {
        if self.grabbed.swap(on, Ordering::SeqCst) == on {
            return;
        }
        for fd in self.fds.lock().unwrap().values() {
            grab_fd(*fd, on);
        }
    }
}

fn grab_fd(fd: RawFd, on: bool) {
    // SAFETY: ioctl em um fd de evdev aberto que pertence a uma thread viva
    // (o fd sai do mapa antes de ser fechado).
    unsafe {
        libc::ioctl(fd, EVIOCGRAB as _, on as libc::c_int);
    }
}

fn is_gamepad(dev: &Device) -> bool {
    let id = dev.input_id();
    // Ignora o controle virtual da Steam (duplicaria os eventos do físico).
    if id.vendor() == 0x28de && id.product() == 0x11ff {
        return false;
    }
    if dev.name().unwrap_or("").contains("Steam Virtual") {
        return false;
    }
    let keys_ok = dev
        .supported_keys()
        .map(|k| k.contains(KeyCode::BTN_SOUTH) || k.contains(KeyCode::BTN_MODE))
        .unwrap_or(false);
    let abs_ok = dev
        .supported_absolute_axes()
        .map(|a| a.contains(AbsoluteAxisCode::ABS_X))
        .unwrap_or(false);
    keys_ok && abs_ok
}

pub fn start(tx: async_channel::Sender<PadEvent>, grabber: Grabber) {
    let seen: Arc<Mutex<HashSet<PathBuf>>> = Arc::default();
    std::thread::Builder::new()
        .name("orbit-pad-scan".into())
        .spawn(move || loop {
            if tx.is_closed() {
                return;
            }
            if let Ok(rd) = std::fs::read_dir("/dev/input") {
                let present: HashSet<PathBuf> = rd
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .map(|n| n.to_string_lossy().starts_with("event"))
                            .unwrap_or(false)
                    })
                    .collect();
                let mut s = seen.lock().unwrap();
                s.retain(|p| present.contains(p));
                for path in present {
                    if s.contains(&path) {
                        continue;
                    }
                    // Falha de permissão = tenta de novo no próximo ciclo (ACL pode demorar).
                    let Ok(dev) = Device::open(&path) else { continue };
                    s.insert(path.clone());
                    if is_gamepad(&dev) {
                        eprintln!("orbit: controle conectado: {}", dev.name().unwrap_or("?"));
                        let (tx, g, seen) = (tx.clone(), grabber.clone(), seen.clone());
                        let _ = std::thread::Builder::new()
                            .name("orbit-pad".into())
                            .spawn(move || read_device(path, dev, tx, g, seen));
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(2));
        })
        .expect("thread do scanner");
}

fn read_device(
    path: PathBuf,
    mut dev: Device,
    tx: async_channel::Sender<PadEvent>,
    grabber: Grabber,
    seen: Arc<Mutex<HashSet<PathBuf>>>,
) {
    use PadEvent::*;

    let fd = dev.as_raw_fd();
    {
        let mut fds = grabber.fds.lock().unwrap();
        fds.insert(path.clone(), fd);
        if grabber.grabbed.load(Ordering::SeqCst) {
            grab_fd(fd, true);
        }
    }

    let mut ranges: HashMap<AbsoluteAxisCode, (i32, i32)> = HashMap::new();
    if let Ok(abs) = dev.get_absinfo() {
        for (code, info) in abs {
            ranges.insert(code, (info.minimum(), info.maximum()));
        }
    }

    // Estado atual de cada eixo direcional: -1, 0, 1
    let (mut hat_x, mut hat_y, mut stick_x, mut stick_y) = (0i32, 0i32, 0i32, 0i32);

    let send = |e: PadEvent| tx.send_blocking(e).is_ok();
    let axis = |cur: &mut i32, new: i32, neg: Dir, pos: Dir| -> bool {
        if *cur == new {
            return true;
        }
        let dir = |v: i32| if v < 0 { neg } else { pos };
        let mut ok = true;
        if *cur != 0 {
            ok &= send(Nav(dir(*cur), false));
        }
        if new != 0 {
            ok &= send(Nav(dir(new), true));
        }
        *cur = new;
        ok
    };

    'outer: loop {
        let events = match dev.fetch_events() {
            Ok(e) => e,
            Err(_) => break, // desconectado
        };
        for ev in events {
            let ok = match ev.destructure() {
                EventSummary::Key(_, code, value) => {
                    if value == 2 {
                        continue; // auto-repeat do kernel
                    }
                    let pressed = value == 1;
                    let e = match code {
                        KeyCode::BTN_MODE if pressed => Some(Guide),
                        KeyCode::BTN_SOUTH if pressed => Some(Accept),
                        KeyCode::BTN_EAST if pressed => Some(Back),
                        KeyCode::BTN_NORTH if pressed => Some(Close),
                        KeyCode::BTN_TL if pressed => Some(PrevTab),
                        KeyCode::BTN_TR if pressed => Some(NextTab),
                        KeyCode::BTN_DPAD_UP => Some(Nav(Dir::Up, pressed)),
                        KeyCode::BTN_DPAD_DOWN => Some(Nav(Dir::Down, pressed)),
                        KeyCode::BTN_DPAD_LEFT => Some(Nav(Dir::Left, pressed)),
                        KeyCode::BTN_DPAD_RIGHT => Some(Nav(Dir::Right, pressed)),
                        _ => None,
                    };
                    e.map(send).unwrap_or(true)
                }
                EventSummary::AbsoluteAxis(_, code, value) => match code {
                    AbsoluteAxisCode::ABS_HAT0X => {
                        axis(&mut hat_x, value.signum(), Dir::Left, Dir::Right)
                    }
                    AbsoluteAxisCode::ABS_HAT0Y => {
                        axis(&mut hat_y, value.signum(), Dir::Up, Dir::Down)
                    }
                    AbsoluteAxisCode::ABS_X | AbsoluteAxisCode::ABS_Y => {
                        let (min, max) = ranges.get(&code).copied().unwrap_or((-32768, 32767));
                        let half = ((max - min) as f32 / 2.0).max(1.0);
                        let n = (value as f32 - (min + max) as f32 / 2.0) / half;
                        let is_x = code == AbsoluteAxisCode::ABS_X;
                        let cur = if is_x { &mut stick_x } else { &mut stick_y };
                        // Histerese: engata em 60%, solta abaixo de 35%.
                        let new = if n > 0.6 {
                            1
                        } else if n < -0.6 {
                            -1
                        } else if n.abs() < 0.35 {
                            0
                        } else {
                            *cur
                        };
                        if is_x {
                            axis(cur, new, Dir::Left, Dir::Right)
                        } else {
                            axis(cur, new, Dir::Up, Dir::Down)
                        }
                    }
                    _ => true,
                },
                _ => true,
            };
            if !ok {
                break 'outer;
            }
        }
    }

    grabber.fds.lock().unwrap().remove(&path);
    seen.lock().unwrap().remove(&path);
    drop(dev);
    eprintln!("orbit: controle desconectado ({})", path.display());
}
