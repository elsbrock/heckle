//! Local TTS (Kokoro via sherpa-onnx) streamed straight into a long-lived `pw-cat` player.

use std::io::Write;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    /// Bumped by `stop`; work and audio from older epochs is dropped.
    epoch: AtomicU64,
}

enum Audio {
    Chunk(u64, Vec<f32>),
    /// Restart the player so already-buffered audio is cut immediately.
    Flush,
}

pub struct Voice {
    tx: mpsc::Sender<(u64, String)>,
    audio: mpsc::Sender<Audio>,
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

        let (player, stdin) = spawn_player(rate)?;
        let state = Arc::new(State {
            busy_until: Mutex::new(Instant::now()),
            pending: AtomicUsize::new(0),
            epoch: AtomicU64::new(0),
        });
        let (audio_tx, audio_rx) = mpsc::channel::<Audio>();
        let pump_state = state.clone();
        thread::spawn(move || pump(audio_rx, player, stdin, rate, pump_state));

        let (tx, rx) = mpsc::channel::<(u64, String)>();
        let worker_state = state.clone();
        let worker_audio = audio_tx.clone();
        thread::spawn(move || {
            let gen_config = GenerationConfig {
                sid,
                speed,
                ..Default::default()
            };
            for (epoch, text) in rx {
                if epoch == worker_state.epoch.load(Ordering::SeqCst) {
                    synthesize(
                        &tts,
                        &gen_config,
                        &text,
                        rate,
                        epoch,
                        &worker_audio,
                        &worker_state,
                    );
                }
                worker_state.pending.fetch_sub(1, Ordering::SeqCst);
            }
        });

        Ok(Self {
            tx,
            audio: audio_tx,
            state,
        })
    }

    /// Queue a clause for synthesis and playback; returns immediately.
    pub fn say(&self, text: String) {
        self.state.pending.fetch_add(1, Ordering::SeqCst);
        let epoch = self.state.epoch.load(Ordering::SeqCst);
        if self.tx.send((epoch, text)).is_err() {
            self.state.pending.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Drop everything queued, abort synthesis in flight and cut the audio already buffered.
    pub fn stop(&self) {
        self.state.epoch.fetch_add(1, Ordering::SeqCst);
        *self.state.busy_until.lock().unwrap() = Instant::now();
        let _ = self.audio.send(Audio::Flush);
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
    epoch: u64,
    audio_tx: &mpsc::Sender<Audio>,
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
            if st.epoch.load(Ordering::SeqCst) != epoch {
                return false; // stopped while synthesizing
            }
            let mut busy = st.busy_until.lock().unwrap();
            *busy = (*busy).max(Instant::now())
                + Duration::from_secs_f32(chunk.len() as f32 / rate as f32);
            tx.send(Audio::Chunk(epoch, chunk.to_vec())).is_ok()
        }),
    );
    if audio.is_none() {
        tracing::warn!(text, "tts generation failed");
    }
}

fn spawn_player(rate: i32) -> Result<(Child, ChildStdin)> {
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
    Ok((player, stdin))
}

/// Owns the player so audio is written in arrival order without blocking synthesis, and so a
/// flush can restart it.
fn pump(
    rx: mpsc::Receiver<Audio>,
    mut player: Child,
    mut stdin: ChildStdin,
    rate: i32,
    state: Arc<State>,
) {
    for msg in rx {
        let restart = match msg {
            Audio::Flush => true,
            Audio::Chunk(epoch, chunk) => {
                if epoch != state.epoch.load(Ordering::SeqCst) {
                    continue;
                }
                let bytes: Vec<u8> = chunk.iter().flat_map(|s| s.to_le_bytes()).collect();
                stdin.write_all(&bytes).is_err()
            }
        };
        if restart {
            let _ = player.kill();
            let _ = player.wait();
            match spawn_player(rate) {
                Ok((p, s)) => (player, stdin) = (p, s),
                Err(e) => {
                    tracing::error!("restarting player failed: {e:#}");
                    return;
                }
            }
        }
    }
    let _ = player.kill();
}
