//! eighty-helper: the root daemon that limits battery charging.
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::Parser;
use eighty_core::paths::{CONFIG_DIR, SOCKET_PATH};
use eighty_helper::control::{restore_all, restore_native};
use eighty_helper::engine::{Deps, Engine, Event};
use eighty_helper::log::log;
use eighty_helper::ipc;
use eighty_helper::os_events::spawn_os_events;
use eighty_helper::store::Store;
use eighty_helper::system::MacSystem;
use eighty_hw::battery::IoRegBattery;
use eighty_hw::native::PowerUi;
use eighty_hw::smc::SmcConnection;

/// Keep charging between limits. Runs as root under launchd.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Write the restore state (CHTE=00000000, CHIE=00, ACLC=00) and exit.
    #[arg(long)]
    restore: bool,
}

fn require_root(what: &str) -> anyhow::Result<()> {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } != 0 {
        anyhow::bail!("{what} needs root: run it with sudo (launchd normally runs the helper as root)");
    }
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Write the restore state; returns how many keys failed.
fn restore(print: impl Fn(&str)) -> anyhow::Result<usize> {
    let mut smc = SmcConnection::open().context("opening AppleSMC")?;
    let errors = restore_all(&mut smc);
    for (key, e) in &errors {
        print(&format!("restore {key}: FAILED: {e}"));
    }
    if errors.is_empty() {
        print("restore: wrote CHTE=00000000, CHIE=00, ACLC=00 to every key present");
    }
    Ok(errors.len())
}

/// `--restore`: SMC keys first, then the user's own native limit. Returns the failure
/// count; an unreadable config counts, since the original limit couldn't be put back.
fn restore_everything(print: impl Fn(&str)) -> anyhow::Result<usize> {
    let mut failed = restore(&print)?;
    let store = Store::new(PathBuf::from(CONFIG_DIR));
    match restore_native(&mut PowerUi::new(), &store) {
        Ok(r) => print(&r.message()),
        // Includes an unreadable config: the original couldn't be put back.
        Err(e) => {
            print(&e.message());
            failed += 1;
        }
    }
    Ok(failed)
}

fn remove_socket(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log(&format!("removing {}: {e}", path.display()));
        }
    }
}

/// After an engine panic: put the keys back so charging is not left inhibited.
///
/// This deliberately ignores the engine's `may_touch_keys` gate (which keeps it off
/// the keys while another limiter is present or the mode is unsupported): the engine's
/// state is gone, so we err toward allowing charging. `restore_all` only writes keys
/// that exist, and only to the restore state.
fn emergency_restore() {
    log("engine thread panicked: restoring SMC keys and exiting");
    if let Err(e) = restore(log) {
        log(&format!("emergency restore failed: {e:#}"));
    }
}

/// Run the daemon until SIGTERM/SIGINT. Returns the process exit code.
fn run_helper() -> anyhow::Result<u8> {
    require_root("running the helper")?;
    log(&format!("eighty-helper {} starting", env!("CARGO_PKG_VERSION")));
    let (tx, rx) = mpsc::channel::<Event>();
    // Install the handler before the engine can write any key. A signal during
    // startup queues Shutdown, which the engine handles (and restores) right after start.
    let signal_tx = tx.clone();
    ctrlc::set_handler(move || {
        let _ = signal_tx.send(Event::Shutdown);
    })
    .context("installing signal handler")?;
    let (started_tx, started_rx) = mpsc::channel::<Result<(), String>>();
    // The engine owns the SMC connection, so open it on the engine thread.
    let engine = std::thread::Builder::new()
        .name("engine".into())
        .spawn(move || {
            let smc = match SmcConnection::open() {
                Ok(s) => s,
                Err(e) => {
                    let _ = started_tx.send(Err(format!("opening AppleSMC: {e}")));
                    return;
                }
            };
            let deps = Deps {
                smc,
                battery: IoRegBattery,
                system: MacSystem,
                store: Store::new(PathBuf::from(CONFIG_DIR)),
                clock: Box::new(unix_now),
                // Not Send: built here, on the engine thread that uses it.
                native: PowerUi::new(),
            };
            let engine = Engine::start(deps);
            let _ = started_tx.send(Ok(()));
            engine.run(rx);
        })
        .context("spawning engine thread")?;

    match started_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            let _ = engine.join();
            anyhow::bail!(e);
        }
        // The sender dropped without a message: the engine panicked while starting.
        Err(_) => {
            let _ = engine.join();
            emergency_restore();
            return Ok(1);
        }
    }

    let socket = Path::new(SOCKET_PATH);
    let connected = connect(socket, &tx);
    if connected.is_err() {
        // The engine may be holding charge inhibited: shut it down so it restores.
        let _ = tx.send(Event::Shutdown);
    }
    let code = match engine.join() {
        Ok(()) => {
            log("eighty-helper stopped");
            0
        }
        // Exit 1 so launchd restarts the helper.
        Err(_) => {
            emergency_restore();
            1
        }
    };
    remove_socket(socket);
    connected.map(|()| code)
}

/// Wire the IPC socket and OS power events to the engine.
fn connect(socket: &Path, tx: &mpsc::Sender<Event>) -> anyhow::Result<()> {
    let listener = ipc::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    let ipc_tx = tx.clone();
    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || ipc::serve(listener, ipc_tx))
        .context("spawning ipc thread")?;
    spawn_os_events(tx.clone()).context("spawning os-events thread")?;
    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = if args.restore {
        require_root("--restore")
            .and_then(|()| restore_everything(|m| println!("{m}")))
            .map(|failed| u8::from(failed > 0))
    } else {
        run_helper()
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("eighty-helper: {e:#}");
            ExitCode::FAILURE
        }
    }
}
