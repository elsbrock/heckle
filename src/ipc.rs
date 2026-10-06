//! Control channel: a line-based protocol over a Unix socket, shared by the CLI and the tray.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, watch};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Set {
    On,
    Off,
    Toggle,
}

impl Set {
    pub fn apply(self, current: bool) -> bool {
        match self {
            Set::On => true,
            Set::Off => false,
            Set::Toggle => !current,
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "on" => Ok(Set::On),
            "off" => Ok(Set::Off),
            "toggle" => Ok(Set::Toggle),
            other => bail!("expected on|off|toggle, got {other:?}"),
        }
    }

    fn wire(self) -> &'static str {
        match self {
            Set::On => "on",
            Set::Off => "off",
            Set::Toggle => "toggle",
        }
    }
}

#[derive(Clone, Debug)]
pub enum Cmd {
    /// Narrate what is on screen now, optionally steered by a hint.
    Poke(Option<String>),
    /// Cut off the current speech.
    Stop,
    /// Continuous narration on/off.
    Auto(Set),
    /// Whole daemon on/off; off also shuts the camera down.
    Enabled(Set),
    Quit,
}

impl Cmd {
    /// Whether this command should abort a narration that is in flight.
    pub fn cancels_narration(&self) -> bool {
        !matches!(self, Cmd::Auto(_))
    }

    pub fn wire(&self) -> String {
        match self {
            Cmd::Poke(None) => "poke".into(),
            Cmd::Poke(Some(h)) => format!("poke {h}"),
            Cmd::Stop => "stop".into(),
            Cmd::Auto(s) => format!("auto {}", s.wire()),
            Cmd::Enabled(s) => format!("enabled {}", s.wire()),
            Cmd::Quit => "quit".into(),
        }
    }

    fn parse(line: &str) -> Result<Self> {
        let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
        let rest = rest.trim();
        Ok(match verb {
            "poke" => Cmd::Poke((!rest.is_empty()).then(|| rest.to_string())),
            "stop" => Cmd::Stop,
            "auto" => Cmd::Auto(Set::parse(rest)?),
            "enabled" => Cmd::Enabled(Set::parse(rest)?),
            "quit" => Cmd::Quit,
            other => bail!("unknown command {other:?}"),
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Status {
    pub enabled: bool,
    pub auto: bool,
}

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join("narrator.sock")
}

/// Send one line to the running daemon and return its one-line reply.
pub async fn send(line: &str) -> Result<String> {
    let mut stream = UnixStream::connect(socket_path())
        .await
        .context("narrator daemon is not running (start it with `narrator`)")?;
    stream.write_all(format!("{line}\n").as_bytes()).await?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).await?;
    Ok(reply.trim().to_string())
}

/// Bind the control socket and forward parsed commands to `tx`.
pub async fn serve(tx: mpsc::UnboundedSender<Cmd>, status: watch::Receiver<Status>) -> Result<()> {
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        bail!(
            "XDG_RUNTIME_DIR is not set; refusing to put the control socket in a shared directory"
        );
    }
    let path = socket_path();
    if path.exists() {
        if UnixStream::connect(&path).await.is_ok() {
            bail!(
                "another narrator daemon is already running ({})",
                path.display()
            );
        }
        std::fs::remove_file(&path).context("removing stale socket")?;
    }
    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    // Only the owner may control the daemon (it triggers screen and webcam captures).
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
        .context("restricting socket permissions")?;
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                // e.g. out of file descriptors: don't spin
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            };
            let (tx, status) = (tx.clone(), status.clone());
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut line = String::new();
                // commands are short; cap the line so a runaway client cannot grow memory
                if BufReader::new(read.take(4096))
                    .read_line(&mut line)
                    .await
                    .is_err()
                {
                    return;
                }
                let line = line.trim();
                let reply = if line == "status" {
                    let s = *status.borrow();
                    format!("enabled={} auto={}", s.enabled, s.auto)
                } else {
                    match Cmd::parse(line) {
                        Ok(cmd) => {
                            let _ = tx.send(cmd);
                            "ok".to_string()
                        }
                        Err(e) => format!("error: {e}"),
                    }
                };
                let _ = write.write_all(format!("{reply}\n").as_bytes()).await;
            });
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_roundtrip() {
        for line in [
            "poke",
            "poke what is he doing",
            "stop",
            "auto toggle",
            "enabled off",
            "quit",
        ] {
            assert_eq!(Cmd::parse(line).unwrap().wire(), line);
        }
        assert!(Cmd::parse("auto maybe").is_err());
        assert!(Cmd::parse("dance").is_err());
    }
}
