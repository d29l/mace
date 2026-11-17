//! `mace overlay-helper`: the overlay's long-running companion, so the panel
//! never waits for a process to start.
//!
//! It reads one JSON request per line on stdin and answers on stdout:
//!
//!     {"cmd": "state"}                 -> {"type": "state", "state": {...}}
//!     {"cmd": "run", "id": 7, "args": ["start", "alt1", "main"]}
//!                                      -> {"type": "done", "id": 7, "ok": true, "error": ""}
//!
//! State requests that arrive while one is being worked out are folded into a
//! single follow-up. Commands run on their own threads. Anything else that
//! prints goes to stderr, so stdout stays the protocol. It exits when stdin
//! closes (the overlay went away).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, Write};
use std::os::fd::FromRawFd;
use std::sync::{Arc, Condvar, Mutex};

use serde_json::{Value, json};

use crate::cli;
use crate::servers::LogFollower;

pub fn serve() -> i32 {
    // the protocol keeps the real stdout; everything else printed lands on stderr
    // SAFETY: plain descriptor juggling on our own stdout/stderr.
    let protocol = unsafe {
        let fd = libc::dup(1);
        libc::dup2(2, 1);
        File::from_raw_fd(fd)
    };
    let out = Arc::new(Mutex::new(protocol));
    let reply = {
        let out = out.clone();
        move |message: Value| {
            let mut out = out.lock().unwrap();
            let _ = writeln!(out, "{message}");
            let _ = out.flush();
        }
    };

    let wanted = Arc::new((Mutex::new(false), Condvar::new()));
    {
        let (wanted, reply) = (wanted.clone(), reply.clone());
        std::thread::spawn(move || {
            let mut followers: HashMap<String, LogFollower> = HashMap::new();
            loop {
                let (lock, signal) = &*wanted;
                let mut pending = lock.lock().unwrap();
                while !*pending {
                    pending = signal.wait(pending).unwrap();
                }
                *pending = false;
                drop(pending);
                reply(json!({"type": "state", "state": cli::overlay_state(Some(&mut followers))}));
            }
        });
    }

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<Value>(&line) else { continue };
        match request.get("cmd").and_then(Value::as_str) {
            Some("state") => {
                let (lock, signal) = &*wanted;
                *lock.lock().unwrap() = true;
                signal.notify_one();
            }
            Some("run") => {
                let reply = reply.clone();
                std::thread::spawn(move || {
                    let args: Vec<String> = request.get("args").and_then(Value::as_array).into_iter().flatten()
                        .map(|a| a.as_str().map_or_else(|| a.to_string(), String::from)).collect();
                    let (ok, error) = run(&args);
                    reply(json!({"type": "done", "id": request.get("id"), "cmd": args.first(), "ok": ok, "error": error}));
                });
            }
            _ => {}
        }
    }
    0
}

fn run(args: &[String]) -> (bool, String) {
    let Some(name) = args.first() else { return (false, "no command".into()) };
    if name == "active" {
        return (cli::mace_running(), String::new());
    }
    if ["watch", "overlay-helper", "supervise", "loading"].contains(&name.as_str()) {
        return (false, format!("not a command the overlay runs: {name}"));
    }
    let Some(command) = cli::command(name) else { return (false, format!("no command {name}")) };
    match command(&args[1..], &mut std::io::sink()) {
        Ok(()) => (true, String::new()),
        Err(error) => (false, format!("{error:#}")),
    }
}
