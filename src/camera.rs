//! Webcam source: a long-lived GStreamer pipeline reading the shared PipeWire camera and
//! emitting low-rate JPEG frames. Only the newest frame is kept, so a look costs nothing.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

type Latest = Arc<Mutex<Option<(Instant, Vec<u8>)>>>;

pub struct Camera {
    latest: Latest,
    _child: Child,
}

impl Camera {
    /// Start the pipeline. `target` is an optional PipeWire node name or serial.
    pub fn start(target: Option<&str>, fps: u32) -> Result<Self> {
        let src = match target {
            Some(t) => format!("pipewiresrc target-object={t}"),
            None => "pipewiresrc".to_string(),
        };
        let pipeline = format!(
            "{src} ! videorate drop-only=true max-rate={fps} \
             ! videoconvert ! jpegenc quality=70 ! fdsink fd=1"
        );
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
        tokio::spawn(async move {
            let (mut buf, mut chunk) = (Vec::new(), vec![0u8; 64 * 1024]);
            loop {
                match stdout.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                }
                while let Some(frame) = take_frame(&mut buf) {
                    *sink.lock().unwrap() = Some((Instant::now(), frame));
                }
            }
            tracing::warn!("camera pipeline ended");
        });

        Ok(Self {
            latest,
            _child: child,
        })
    }

    /// Wait until the first frame has arrived (the pipeline takes a moment to negotiate).
    pub async fn wait_ready(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
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
