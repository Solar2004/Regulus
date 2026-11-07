//! A program in a pseudo-terminal (`portable-pty`, docs/06-stack.md §3): one thread reads
//! what it prints, one writes what it is sent, one waits for it to end.

use std::io::{self, Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};

use crate::exit::Exit;
use crate::run::{Spec, THREAD};

/// A live terminal: what is typed goes through `input`; dropping it ends the writer and
/// closes the terminal's side of the pair.
pub(crate) struct Terminal {
    pub input: mpsc::Sender<Vec<u8>>,
    _master: Box<dyn MasterPty + Send>,
}

/// What `open` hands back besides the terminal: the child to wait on and the reader.
pub(crate) struct Opened {
    pub terminal: Terminal,
    pub child: Box<dyn Child + Send + Sync>,
    pub reader: Box<dyn Read + Send>,
}

fn other(error: impl ToString) -> io::Error {
    io::Error::other(error.to_string())
}

/// Spawns `spec` in a new terminal of `rows` × `cols`. The child leads its own session
/// (portable-pty calls `setsid`), so its pid is its process group.
pub(crate) fn open(spec: &Spec, rows: u16, cols: u16) -> io::Result<(u32, Opened)> {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(other)?;
    let mut command = CommandBuilder::new(&spec.program);
    command.args(&spec.args);
    command.cwd(&spec.cwd);
    for (name, value) in &spec.env {
        command.env(name, value);
    }
    let child = pair.slave.spawn_command(command).map_err(other)?;
    // Only the child holds the slave now: when it ends, reads on the master end too.
    drop(pair.slave);
    let pid = child
        .process_id()
        .ok_or_else(|| other("the terminal's program has no pid"))?;
    let reader = pair.master.try_clone_reader().map_err(other)?;
    let mut writer = pair.master.take_writer().map_err(other)?;
    let (input, typed) = mpsc::channel::<Vec<u8>>();
    // Writing can block while the program does not read: never on the bus thread.
    thread::Builder::new().name(THREAD.into()).spawn(move || {
        for bytes in typed {
            if writer
                .write_all(&bytes)
                .and_then(|()| writer.flush())
                .is_err()
            {
                break;
            }
        }
    })?;
    Ok((
        pid,
        Opened {
            terminal: Terminal {
                input,
                _master: pair.master,
            },
            child,
            reader,
        },
    ))
}

/// Reads `reader` until the terminal closes, handing out text chunks of at most `chunk`
/// bytes. A character split between reads waits for its other half.
pub(crate) fn pump(mut reader: Box<dyn Read + Send>, chunk: usize, mut out: impl FnMut(String)) {
    let mut buf = vec![0u8; chunk.max(4)];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        let n = match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        carry.extend_from_slice(&buf[..n]);
        let keep = match std::str::from_utf8(&carry) {
            Ok(_) => 0,
            // Cut in the middle of a character: hold the start of it for the next read.
            Err(e) if e.error_len().is_none() => carry.len() - e.valid_up_to(),
            Err(_) => 0,
        };
        let rest = carry.split_off(carry.len() - keep);
        if !carry.is_empty() {
            out(String::from_utf8_lossy(&carry).into_owned());
        }
        carry = rest;
    }
    if !carry.is_empty() {
        out(String::from_utf8_lossy(&carry).into_owned());
    }
}

/// Waits for the terminal's program and says how it ended.
pub(crate) fn wait(mut child: Box<dyn Child + Send + Sync>, started: Instant) -> Exit {
    let status = child.wait();
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match status {
        Err(e) => Exit {
            ms,
            ..Exit::not_started(e)
        },
        Ok(status) => Exit {
            // portable-pty reports a signal by name, with code 1.
            code: status
                .signal()
                .is_none()
                .then(|| i32::try_from(status.exit_code()).unwrap_or(i32::MAX)),
            ms,
            stderr: status.signal().map(str::to_string).unwrap_or_default(),
            ..Exit::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pump_never_splits_a_character() {
        // "é" is two bytes; a reader that hands out one byte at a time still yields it whole.
        struct OneByte(Vec<u8>, usize);
        impl Read for OneByte {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.1 >= self.0.len() {
                    return Ok(0);
                }
                buf[0] = self.0[self.1];
                self.1 += 1;
                Ok(1)
            }
        }
        let mut chunks = Vec::new();
        pump(Box::new(OneByte("aé".as_bytes().to_vec(), 0)), 16, |c| {
            chunks.push(c)
        });
        assert_eq!(chunks.concat(), "aé");
        assert!(chunks.iter().all(|c| !c.contains('\u{FFFD}')));
    }
}
