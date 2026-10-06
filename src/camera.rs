//! Webcam source: a long-lived GStreamer pipeline reading the shared PipeWire camera and
//! emitting low-rate JPEG frames. Only the newest frame is kept, so a look costs nothing.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

type Latest = Arc<Mutex<Option<(Instant, Vec<u8>)>>>;

/// How the camera delivers video. Built-in webcams usually offer raw frames; many USB cameras
/// only offer MJPEG/H.264.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Raw,
    Mjpeg,
}

pub struct Camera {
    latest: Latest,
    alive: Arc<AtomicBool>,
    _child: Child,
}

/// A camera found in the PipeWire graph.
pub struct Found {
    pub name: String,
    pub description: String,
    pub mode: Mode,
}

/// Find a PipeWire camera whose name or description contains `query` (case-insensitive), and
/// decide from its advertised formats how to read it.
pub async fn resolve(query: &str) -> Result<Found> {
    let out = Command::new("pw-dump")
        .output()
        .await
        .context("running pw-dump (is pipewire on PATH?)")?;
    let nodes: Vec<serde_json::Value> =
        serde_json::from_slice(&out.stdout).context("parsing pw-dump")?;
    let cameras: Vec<(&serde_json::Value, String, String)> = nodes
        .iter()
        .filter_map(|n| {
            let p = &n["info"]["props"];
            (p["media.class"] == "Video/Source").then(|| {
                let get = |k: &str| p[k].as_str().unwrap_or_default().to_string();
                (n, get("node.name"), get("node.description"))
            })
        })
        .collect();
    let q = query.to_lowercase();
    let Some((node, name, description)) = cameras.iter().find(|(_, name, desc)| {
        name.to_lowercase().contains(&q) || desc.to_lowercase().contains(&q)
    }) else {
        let available: Vec<&str> = cameras.iter().map(|(_, _, d)| d.as_str()).collect();
        bail!("no camera matches {query:?}; available: {available:?}")
    };
    let formats = node["info"]["params"]["EnumFormat"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let offers = |sub: &str| formats.iter().any(|f| f["mediaSubtype"] == sub);
    let mode = if offers("raw") {
        Mode::Raw
    } else if offers("mjpg") {
        Mode::Mjpeg
    } else {
        bail!("{description} offers neither raw nor MJPEG video")
    };
    Ok(Found {
        name: name.clone(),
        description: description.clone(),
        mode,
    })
}

impl Camera {
    /// Start the pipeline. `target` is an optional PipeWire node name or serial.
    pub fn start(target: Option<&str>, fps: u32, mode: Mode) -> Result<Self> {
        let src = match target {
            Some(t) => format!("pipewiresrc target-object={t}"),
            None => "pipewiresrc".to_string(),
        };
        // Frames are only decoded, thinned and re-encoded here; scaling is done by the consumer
        // (scaling in the pipeline distorts the aspect ratio when only a width is pinned).
        let decode = match mode {
            Mode::Raw => "",
            Mode::Mjpeg => "! image/jpeg ! jpegdec",
        };
        let pipeline = format!(
            "{src} {decode} ! videorate drop-only=true max-rate={fps} \
             ! videoconvert ! jpegenc quality=70 ! fdsink fd=1"
        );
        tracing::debug!(%pipeline, "starting camera pipeline");
        let mut child = Command::new("gst-launch-1.0")
            .arg("-q")
            .args(pipeline.split_whitespace())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("spawning gst-launch-1.0 (is gstreamer on PATH?)")?;
        let mut stdout = child.stdout.take().context("gst stdout")?;

        let latest: Latest = Arc::default();
        let sink = latest.clone();
        let alive = Arc::new(AtomicBool::new(true));
        let reader_alive = alive.clone();
        tokio::spawn(async move {
            let (mut buf, mut chunk) = (Vec::new(), vec![0u8; 64 * 1024]);
            loop {
                match stdout.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        tracing::trace!(n, buffered = buf.len(), "camera bytes");
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
                while let Some(frame) = take_frame(&mut buf) {
                    *sink.lock().unwrap() = Some((Instant::now(), frame));
                }
            }
            reader_alive.store(false, Ordering::SeqCst);
            tracing::warn!("camera pipeline ended");
        });

        Ok(Self {
            latest,
            alive,
            _child: child,
        })
    }

    /// Wait until the first frame has arrived (the pipeline takes a moment to negotiate).
    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline && self.alive.load(Ordering::SeqCst) {
            if self.latest.lock().unwrap().is_some() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        false
    }

    /// The newest frame, if one arrived within `max_age`.
    pub fn latest(&self, max_age: Duration) -> Option<Vec<u8>> {
        let guard = self.latest.lock().unwrap();
        let (at, frame) = guard.as_ref()?;
        (at.elapsed() <= max_age).then(|| frame.clone())
    }
}

/// Remove and return the first complete JPEG (SOI..EOI) from `buf`, dropping leading garbage.
/// `jpegenc` byte-stuffs 0xFF in entropy data, so the first EOI after SOI ends the frame.
fn take_frame(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let start = buf.windows(2).position(|w| w == [0xFF, 0xD8])?;
    let end = buf[start + 2..]
        .windows(2)
        .position(|w| w == [0xFF, 0xD9])?
        + start
        + 4;
    let frame = buf[start..end].to_vec();
    buf.drain(..end);
    Some(frame)
}

#[cfg(test)]
mod tests {
    use super::take_frame;

    #[test]
    fn extracts_complete_frames_in_order() {
        let mut buf = vec![0x00, 0xFF, 0xD8, 1, 2, 0xFF, 0xD9, 0xFF, 0xD8, 3];
        assert_eq!(
            take_frame(&mut buf),
            Some(vec![0xFF, 0xD8, 1, 2, 0xFF, 0xD9])
        );
        assert_eq!(take_frame(&mut buf), None); // second frame incomplete
        assert_eq!(buf, vec![0xFF, 0xD8, 3]);
    }
}
