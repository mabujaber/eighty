//! eighty-helper: the root daemon that limits battery charging.
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use clap::Parser;
use eighty_core::paths::{CONFIG_DIR, SOCKET_PATH};
use eighty_helper::control::restore_all;
use eighty_helper::engine::{Deps, Engine, Event};
use eighty_helper::log::log;
use eighty_helper::ipc;
use eighty_helper::os_events::spawn_os_events;
use eighty_helper::store::Store;
use eighty_helper::system::MacSystem;
use eighty_hw::battery::IoRegBattery;
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

fn remove_socket(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log(&format!("removing {}: {e}", path.display()));
        }
    }
}

/// After an engine panic: put the keys back so charging is not left inhibited.
fn emergency_restore() {
    if let Err(e) = restore(log) {
        log(&format!("emergency restore failed: {e:#}"));
    }
}

/// Run the daemon until SIGTERM/SIGINT. Returns the process exit code.
fn run_helper() -> anyhow::Result<u8> {
    require_root("running the helper")?;
    log(&format!("eighty-helper {} starting", env!("CARGO_PKG_VERSION")));
    let (tx, rx) = mpsc::channel::<Event>();
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
            return Ok(engine_panicked());
        }
    }

    let socket = Path::new(SOCKET_PATH);
    if let Err(e) = connect(socket, &tx) {
        // The engine may be holding charge inhibited: shut it down so it restores.
        let _ = tx.send(Event::Shutdown);
        if engine.join().is_err() {
            engine_panicked();
        }
        remove_socket(socket);
        return Err(e);
    }

    match engine.join() {
        Ok(()) => {
            log("eighty-helper stopped");
            remove_socket(socket);
            Ok(0)
        }
        Err(_) => Ok(engine_panicked()),
    }
}

/// Wire the IPC socket, OS power events and termination signals to the engine.
fn connect(socket: &Path, tx: &mpsc::Sender<Event>) -> anyhow::Result<()> {
    let listener = ipc::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    let ipc_tx = tx.clone();
    std::thread::Builder::new()
        .name("ipc".into())
        .spawn(move || ipc::serve(listener, ipc_tx))
        .context("spawning ipc thread")?;
    spawn_os_events(tx.clone());
    let signal_tx = tx.clone();
    ctrlc::set_handler(move || {
        let _ = signal_tx.send(Event::Shutdown);
    })
    .context("installing signal handler")?;
    Ok(())
}

/// Returns exit code 1 so launchd restarts the helper.
fn engine_panicked() -> u8 {
    log("engine thread panicked: restoring SMC keys and exiting");
    emergency_restore();
    remove_socket(Path::new(SOCKET_PATH));
    1
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = if args.restore {
        require_root("--restore")
            .and_then(|()| restore(|m| println!("{m}")))
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
