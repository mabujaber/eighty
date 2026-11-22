//! eighty: command-line client for the eighty charge-limit helper.
use clap::{Parser, Subcommand, ValueEnum};
use eighty_cli::install;
use eighty_cli::schedule_args::parse_schedule;
use eighty_cli::client::{call, ClientError};
use eighty_cli::render::{render_doctor, render_status};
use eighty_core::paths::{LOG_PATH, SOCKET_PATH};
use eighty_core::proto::{Request, Response, Status};
use std::io::ErrorKind;
use std::path::Path;
use std::process::ExitCode;

#[derive(Parser)]
#[command(version, about = "Keep your Mac's battery at 80%")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum OnOff {
    On,
    Off,
}

#[derive(Clone, Copy, ValueEnum)]
enum ChargeMode {
    Full,
    Auto,
}

#[derive(Subcommand)]
enum CalAction {
    /// Stop a running calibration and return to the limit.
    Cancel,
    /// Set or clear the calibration schedule: every <N> days [at HH:MM] | off
    Schedule {
        #[arg(trailing_var_arg = true, required = true)]
        words: Vec<String>,
    },
}

#[derive(Subcommand)]
enum Cmd {
    /// Show battery, limit and helper state.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Set the upper limit and optionally the resume (lower) limit.
    Limit { upper: u8, lower: Option<u8> },
    /// Turn auto-discharge on or off (hold mode only).
    AutoDischarge { state: OnOff },
    /// Charge to 100% until unplugged, or return to the limit.
    Charge { mode: ChargeMode },
    /// Discharge to the given percentage.
    Discharge { percent: u8 },
    /// Run a full calibration cycle now (15% → 100% → 1 h → limit).
    Calibrate {
        #[command(subcommand)]
        action: Option<CalAction>,
    },
    /// Diagnose SMC support and helper health.
    Doctor {
        #[arg(long)]
        json: bool,
        /// Clear a recorded fault and try again.
        #[arg(long)]
        retry: bool,
        /// Keep going even though another charge limiter is installed.
        #[arg(long)]
        accept_other_limiters: bool,
    },
    /// Print the last lines of the helper log.
    Logs {
        #[arg(short = 'n', default_value_t = 100)]
        lines: usize,
    },
    /// Install the helper (needs sudo).
    Install,
    /// Remove the helper (needs sudo).
    Uninstall {
        /// Also delete settings and logs.
        #[arg(long)]
        purge: bool,
    },
}

fn send(req: Request) -> Result<Response, ExitCode> {
    call(Path::new(SOCKET_PATH), &req).map_err(|e| {
        eprintln!("{e}");
        match e {
            ClientError::NotRunning => ExitCode::from(2),
            _ => ExitCode::from(1),
        }
    })
}

fn print_json<T: serde::Serialize>(v: &T) -> Result<(), ExitCode> {
    match serde_json::to_string_pretty(v) {
        Ok(t) => {
            println!("{t}");
            Ok(())
        }
        Err(e) => {
            eprintln!("eighty: can't encode JSON: {e}");
            Err(ExitCode::from(1))
        }
    }
}

/// Send `req`, print the status reply, and exit with `code(status)` on success.
fn run_status(req: Request, json: bool, code: fn(&Status) -> u8) -> ExitCode {
    match send(req) {
        Ok(Response::Status(s)) => {
            if json {
                if let Err(c) = print_json(&s) {
                    return c;
                }
            } else {
                print!("{}", render_status(&s));
            }
            ExitCode::from(code(&s))
        }
        Ok(other) => unexpected(other),
        Err(c) => c,
    }
}

fn expect_status(req: Request) -> ExitCode {
    run_status(req, false, |_| 0)
}

fn unexpected(r: Response) -> ExitCode {
    match r {
        Response::Error(m) => eprintln!("eighty: {m}"),
        _ => eprintln!("eighty: unexpected reply from the helper"),
    }
    ExitCode::from(1)
}

fn status(json: bool) -> ExitCode {
    run_status(Request::Status, json, |s| if s.health.is_ok() { 0 } else { 3 })
}

fn doctor(json: bool, retry: bool, accept: bool) -> ExitCode {
    for (wanted, req) in [(retry, Request::ClearFault), (accept, Request::AcceptOtherLimiters)] {
        if wanted {
            match send(req) {
                Ok(Response::Error(m)) => return unexpected(Response::Error(m)),
                Ok(_) => {}
                Err(c) => return c,
            }
        }
    }
    match send(Request::Doctor) {
        Ok(Response::Doctor(d)) => {
            if json {
                if let Err(c) = print_json(&d) {
                    return c;
                }
            } else {
                print!("{}", render_doctor(&d));
            }
            ExitCode::SUCCESS
        }
        Ok(other) => unexpected(other),
        Err(c) => c,
    }
}

fn logs(n: usize) -> ExitCode {
    match std::fs::read(LOG_PATH) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let all: Vec<&str> = text.lines().collect();
            for l in &all[all.len().saturating_sub(n)..] {
                println!("{l}");
            }
            ExitCode::SUCCESS
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            println!("no log yet at {LOG_PATH}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("eighty: can't read {LOG_PATH}: {e}");
            ExitCode::from(1)
        }
    }
}

fn finish(r: anyhow::Result<()>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("eighty: {e:#}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    match Cli::parse().cmd {
        Cmd::Status { json } => status(json),
        Cmd::Limit { upper, lower } => expect_status(Request::SetLimits { upper, lower }),
        Cmd::AutoDischarge { state } => {
            expect_status(Request::SetAutoDischarge { on: matches!(state, OnOff::On) })
        }
        Cmd::Charge { mode: ChargeMode::Full } => expect_status(Request::ChargeFull),
        Cmd::Charge { mode: ChargeMode::Auto } => expect_status(Request::ChargeAuto),
        Cmd::Discharge { percent } => expect_status(Request::DischargeTo { percent }),
        Cmd::Calibrate { action: None } => expect_status(Request::Calibrate),
        Cmd::Calibrate { action: Some(CalAction::Cancel) } => expect_status(Request::CalibrateCancel),
        Cmd::Calibrate { action: Some(CalAction::Schedule { words }) } => match parse_schedule(&words) {
            Ok(req) => expect_status(req),
            Err(m) => {
                eprintln!("{m}");
                ExitCode::from(2)
            }
        },
        Cmd::Doctor { json, retry, accept_other_limiters } => doctor(json, retry, accept_other_limiters),
        Cmd::Logs { lines } => logs(lines),
        Cmd::Install => finish(install::install()),
        Cmd::Uninstall { purge } => finish(install::uninstall(purge)),
    }
}
