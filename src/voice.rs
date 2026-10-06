//! Local TTS (Kokoro via sherpa-onnx) streamed straight into a long-lived `pw-cat` player.

use std::io::Write;
use std::path::Path;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sherpa_onnx::{
    GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig,
    OfflineTtsModelConfig,
};

/// Playback bookkeeping shared between the synthesis worker and the caller.
struct State {
    /// When the audio queued so far will finish playing.
    busy_until: Mutex<Instant>,
    /// Clauses handed to `say` that have not finished synthesizing.
    pending: AtomicUsize,
}

pub struct Voice {
    tx: mpsc::Sender<String>,
    state: Arc<State>,
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
        let (audio_tx, audio_rx) = mpsc::channel::<Vec<f32>>();
        thread::spawn(move || pump(audio_rx, stdin));

        let state = Arc::new(State {
            busy_until: Mutex::new(Instant::now()),
            pending: AtomicUsize::new(0),
        });
        let (tx, rx) = mpsc::channel::<String>();
        let worker_state = state.clone();
        thread::spawn(move || {
            let _player = player; // keep the player alive for the worker's lifetime
            let gen_config = GenerationConfig {
                sid,
                speed,
                ..Default::default()
            };
            for text in rx {
                synthesize(&tts, &gen_config, &text, rate, &audio_tx, &worker_state);
                worker_state.pending.fetch_sub(1, Ordering::SeqCst);
            }
        });

        Ok(Self { tx, state })
    }

    /// Queue a clause for synthesis and playback; returns immediately.
    pub fn say(&self, text: String) {
        self.state.pending.fetch_add(1, Ordering::SeqCst);
        if self.tx.send(text).is_err() {
            self.state.pending.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// True once everything queued is synthesized and at most `lookahead` of audio remains.
    pub fn ready_within(&self, lookahead: Duration) -> bool {
        self.state.pending.load(Ordering::SeqCst) == 0
            && self
                .state
                .busy_until
                .lock()
                .unwrap()
                .saturating_duration_since(Instant::now())
                <= lookahead
    }
}

fn synthesize(
    tts: &OfflineTts,
    config: &GenerationConfig,
    text: &str,
    rate: i32,
    audio_tx: &mpsc::Sender<Vec<f32>>,
    state: &Arc<State>,
) {
    let start = Instant::now();
    let (tx, st) = (audio_tx.clone(), state.clone());
    let mut first = true;
    let audio = tts.generate_with_config(
        text,
        config,
        Some(move |chunk: &[f32], _progress: f32| {
            if first {
                tracing::info!(ms = start.elapsed().as_millis() as u64, "first audio chunk");
                first = false;
            }
            let mut busy = st.busy_until.lock().unwrap();
            *busy = (*busy).max(Instant::now())
                + Duration::from_secs_f32(chunk.len() as f32 / rate as f32);
            tx.send(chunk.to_vec()).is_ok()
        }),
    );
    if audio.is_none() {
        tracing::warn!(text, "tts generation failed");
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
