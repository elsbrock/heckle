//! Local TTS (Kokoro via sherpa-onnx) streamed straight into a long-lived `pw-cat` player.

use std::io::Write;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use anyhow::{Context, Result};
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

pub struct Voice {
    tts: OfflineTts,
    sid: i32,
    speed: f32,
    audio: mpsc::Sender<Vec<f32>>,
    _player: Child,
}

impl Voice {
    pub fn new(model_dir: &Path, sid: i32, speed: f32) -> Result<Self> {
        let p = |f: &str| Some(model_dir.join(f).to_string_lossy().into_owned());
        let config = OfflineTtsConfig {
            model: OfflineTtsModelConfig {
                kokoro: OfflineTtsKokoroModelConfig {
                    model: p("model.onnx"),
                    voices: p("voices.bin"),
                    tokens: p("tokens.txt"),
                    data_dir: p("espeak-ng-data"),
                    ..Default::default()
                },
                num_threads: 4,
                ..Default::default()
            },
            max_num_sentences: 1,
            ..Default::default()
        };
        let tts = OfflineTts::create(&config).context("loading kokoro model")?;
        let rate = tts.sample_rate();

        let mut player = Command::new("pw-cat")
            .args([
                "-p",
                "--raw",
                "--format",
                "f32",
                "--channels",
                "1",
                "--rate",
                &rate.to_string(),
                "-",
            ])
            .stdin(Stdio::piped())
            .spawn()
            .context("spawning pw-cat")?;
        let stdin = player.stdin.take().context("pw-cat stdin")?;
        let (audio, rx) = mpsc::channel::<Vec<f32>>();
        thread::spawn(move || pump(rx, stdin));

        Ok(Self {
            tts,
            sid,
            speed,
            audio,
            _player: player,
        })
    }

    /// Synthesize `text`; each finished sentence chunk is queued for playback immediately.
    /// Blocks until synthesis (not playback) is done.
    pub fn speak(&self, text: &str) -> Result<()> {
        let start = Instant::now();
        let tx = self.audio.clone();
        let mut first = true;
        let config = GenerationConfig {
            sid: self.sid,
            speed: self.speed,
            ..Default::default()
        };
        let audio = self
            .tts
            .generate_with_config(
                text,
                &config,
                Some(move |chunk: &[f32], _progress: f32| {
                    if first {
                        tracing::info!(
                            ms = start.elapsed().as_millis() as u64,
                            samples = chunk.len(),
                            "first audio chunk"
                        );
                        first = false;
                    }
                    tx.send(chunk.to_vec()).is_ok()
                }),
            )
            .context("tts generation failed")?;
        tracing::info!(
            ms = start.elapsed().as_millis() as u64,
            samples = audio.samples().len(),
            secs = audio.samples().len() as f32 / audio.sample_rate() as f32,
            "synthesis done"
        );
        Ok(())
    }
}

/// Owns the player's stdin so audio is written in arrival order without blocking synthesis.
fn pump(rx: mpsc::Receiver<Vec<f32>>, mut stdin: ChildStdin) {
    for chunk in rx {
        let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
        if stdin.write_all(&bytes).is_err() {
            break;
        }
    }
}
