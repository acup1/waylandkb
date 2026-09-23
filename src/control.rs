use std::fs;
use std::io::Read;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::mpsc::{self, Receiver};
use std::thread;

use anyhow::{Context, Result};

#[derive(Clone, Copy, Debug)]
pub enum VisibilityCommand {
    Show,
    Hide,
    Toggle,
}

pub fn start_socket(path: &Path) -> Result<Receiver<VisibilityCommand>> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("failed to remove stale socket {path:?}"))?;
    }

    let listener = UnixListener::bind(path)
        .with_context(|| format!("failed to bind control socket {path:?}"))?;
    let (sender, receiver) = mpsc::channel();

    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else {
                continue;
            };

            let mut message = String::new();
            if stream.read_to_string(&mut message).is_err() {
                continue;
            }

            let command = match message.trim() {
                "show" => VisibilityCommand::Show,
                "hide" => VisibilityCommand::Hide,
                "toggle" => VisibilityCommand::Toggle,
                _ => continue,
            };

            if sender.send(command).is_err() {
                break;
            }
        }
    });

    Ok(receiver)
}
