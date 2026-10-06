mod brain;
mod camera;
mod ipc;
mod screen;
mod tray;
mod voice;

use std::io::Cursor;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use image::{ImageFormat, imageops::FilterType};
use tokio::sync::{mpsc, watch};

const SYSTEM: &str = "You are Sir David Attenborough narrating a nature documentary. The subject is a human \
at its workstation. You get the screen and, when available, a webcam image of the human. \
Observe their behaviour, mood and what is on screen with hushed wonder and dry British wit. \
Speak in ONE sentence of at most 15 words, present tense, as spoken narration: no markdown, no emoji. \
Be specific to what you see and never repeat your earlier lines. \
Never read out passwords, tokens or private message contents. \
If nothing has meaningfully changed, reply exactly: SILENCE";

#[derive(Parser)]
#[command(about = "Live documentary-style commentator for your screen and webcam")]
struct Cli {
    #[command(flatten)]
    args: Args,
    /// Control a running daemon; without a subcommand, run the daemon.
    #[command(subcommand)]
    cmd: Option<Sub>,
}

#[derive(Clone, Copy, ValueEnum)]
enum SetArg {
    On,
    Off,
    Toggle,
}

impl From<SetArg> for ipc::Set {
    fn from(s: SetArg) -> Self {
        match s {
            SetArg::On => ipc::Set::On,
            SetArg::Off => ipc::Set::Off,
            SetArg::Toggle => ipc::Set::Toggle,
        }
    }
}

#[derive(Subcommand)]
enum Sub {
    /// Narrate what is on screen right now (interrupts current speech).
    Poke {
        /// Optional hint to steer the line.
        hint: Vec<String>,
    },
    /// Cut off the current speech.
    Stop,
    /// Continuous narration.
    Auto {
        set: SetArg,
    },
    /// Show or hide a window with the live webcam.
    Preview {
        set: SetArg,
    },
    /// Turn the whole daemon on or off (off also stops the camera).
    Enable,
    Disable,
    /// Flip enabled/disabled; handy as a single hotkey.
    Toggle,
    Status,
    Quit,
}

#[derive(clap::Args)]
struct Args {
    #[arg(long, env = "OPENROUTER_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    #[arg(long, default_value = "anthropic/claude-haiku-4.5")]
    model: String,
    /// Longest edge of each screen tile sent to the model (Claude downsizes beyond ~1568).
    #[arg(long, default_value_t = 1568)]
    width: u32,
    /// Split the screen into this many vertical tiles so text stays readable on wide displays
    /// (0 = automatic, one tile per ~2560 px of width).
    #[arg(long, default_value_t = 0)]
    tiles: u32,
    /// Start in continuous mode (default: idle until `narrator poke`).
    #[arg(long)]
    auto: bool,
    /// Open the webcam preview window at startup.
    #[arg(long)]
    preview: bool,
    /// Disable the webcam (screen only).
    #[arg(long)]
    no_camera: bool,
    /// Camera to use: part of its name (e.g. "insta"); default is PipeWire's default source.
    #[arg(long, env = "NARRATOR_CAMERA")]
    camera_target: Option<String>,
    /// Start the next look when this much audio is left, to hide the model's latency.
    #[arg(long, default_value_t = 1500)]
    lookahead_ms: u64,
    /// Speak this text with the local voice and exit (voice spike).
    #[arg(long)]
    say: Option<String>,
    /// Kokoro model directory.
    #[arg(long, default_value = "~/.cache/narrator/models/kokoro-en-v0_19")]
    voice_dir: String,
    /// Kokoro speaker id (9 = bm_george, 10 = bm_lewis, 5 = am_adam).
    #[arg(long, default_value_t = 9)]
    sid: i32,
    #[arg(long, default_value_t = 1.0)]
    speed: f32,
}

fn to_jpeg(img: &image::RgbImage, edge: u32) -> Result<Vec<u8>> {
    let (w, h) = img.dimensions();
    let scale = (edge as f32 / w.max(h) as f32).min(1.0);
    let mut out = Cursor::new(Vec::new());
    if scale < 1.0 {
        image::imageops::resize(
            img,
            (w as f32 * scale) as u32,
            (h as f32 * scale) as u32,
            FilterType::Triangle,
        )
        .write_to(&mut out, ImageFormat::Jpeg)?;
    } else {
        img.write_to(&mut out, ImageFormat::Jpeg)?;
    }
    Ok(out.into_inner())
}

/// Decode a JPEG and re-encode it with its longest edge at `edge`.
fn shrink(jpeg: &[u8], edge: u32) -> Result<Vec<u8>> {
    let img = image::load_from_memory_with_format(jpeg, ImageFormat::Jpeg)?.to_rgb8();
    to_jpeg(&img, edge)
}

/// Widest tile (in physical pixels) before the frame is split: wider tiles would be downscaled
/// below ~0.6x when sent, which makes small text unreadable.
const MAX_TILE_WIDTH: u32 = 2560;

/// Cut the frame into vertical tiles (left to right) and encode each, in parallel.
fn screen_tiles(img: &image::RgbImage, edge: u32, count: u32) -> Result<Vec<(String, Vec<u8>)>> {
    let (w, h) = img.dimensions();
    let n = if count == 0 {
        w.div_ceil(MAX_TILE_WIDTH).max(1)
    } else {
        count
    };
    let jpegs = std::thread::scope(|s| {
        let handles: Vec<_> = (0..n)
            .map(|i| {
                s.spawn(move || {
                    let (x0, x1) = (w * i / n, w * (i + 1) / n);
                    let tile = image::imageops::crop_imm(img, x0, 0, x1 - x0, h).to_image();
                    to_jpeg(&tile, edge)
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("tile encoder panicked"))
            .collect::<Result<Vec<_>>>()
    })?;
    Ok(jpegs
        .into_iter()
        .enumerate()
        .map(|(i, jpeg)| {
            let label = if n == 1 {
                "Screen".to_string()
            } else {
                format!("Screen, part {}/{n} (left to right)", i + 1)
            };
            (label, jpeg)
        })
        .collect())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "narrator=info".into()),
        )
        .init();
    let cli = Cli::parse();
    let line = match cli.cmd {
        None => return run(cli.args).await,
        Some(Sub::Poke { hint }) => {
            ipc::Cmd::Poke((!hint.is_empty()).then(|| hint.join(" "))).wire()
        }
        Some(Sub::Stop) => ipc::Cmd::Stop.wire(),
        Some(Sub::Auto { set }) => ipc::Cmd::Auto(set.into()).wire(),
        Some(Sub::Preview { set }) => ipc::Cmd::Preview(set.into()).wire(),
        Some(Sub::Enable) => ipc::Cmd::Enabled(ipc::Set::On).wire(),
        Some(Sub::Disable) => ipc::Cmd::Enabled(ipc::Set::Off).wire(),
        Some(Sub::Toggle) => ipc::Cmd::Enabled(ipc::Set::Toggle).wire(),
        Some(Sub::Quit) => ipc::Cmd::Quit.wire(),
        Some(Sub::Status) => "status".to_string(),
    };
    let reply = ipc::send(&line).await?;
    println!("{reply}");
    if reply.starts_with("error") {
        std::process::exit(1);
    }
    Ok(())
}

struct Ctx {
    brain: brain::Brain,
    voice: voice::Voice,
    width: u32,
    tiles: u32,
}

/// Which camera to use: `(PipeWire node name, description, how to read it)`.
async fn pick_camera(args: &Args) -> Option<(Option<String>, String, camera::Mode)> {
    if args.no_camera {
        return None;
    }
    match args.camera_target.as_deref() {
        Some(q) => match camera::resolve(q).await {
            Ok(f) => Some((Some(f.name), f.description, f.mode)),
            Err(e) => {
                tracing::warn!("camera unavailable: {e:#}");
                None
            }
        },
        None => Some((None, "default camera".to_string(), camera::Mode::Raw)),
    }
}

/// Open, close or toggle the preview window, keeping `status.preview` truthful.
async fn set_preview(
    args: &Args,
    preview: &mut Option<camera::Preview>,
    status: &mut ipc::Status,
    set: ipc::Set,
) {
    let want = set.apply(status.preview);
    *preview = None; // closes any existing window
    status.preview = false;
    if !want || !status.enabled {
        return;
    }
    let Some((target, label, mode)) = pick_camera(args).await else {
        tracing::warn!("preview unavailable: no usable camera");
        return;
    };
    match camera::Preview::start(target.as_deref(), mode) {
        Ok(p) => {
            tracing::info!(camera = %label, "preview opened");
            *preview = Some(p);
            status.preview = true;
        }
        Err(e) => tracing::warn!("preview unavailable: {e:#}"),
    }
}

async fn start_camera(args: &Args) -> Option<camera::Camera> {
    let (target, label, mode) = pick_camera(args).await?;
    let t = Instant::now();
    match camera::Camera::start(target.as_deref(), 2, mode) {
        // Some USB cameras take several seconds to wake up and deliver a first frame.
        Ok(c) if c.wait_ready(Duration::from_secs(15)).await => {
            tracing::info!(camera = %label, ?mode, ms = t.elapsed().as_millis() as u64, "camera ready");
            Some(c)
        }
        Ok(_) => {
            tracing::warn!(camera = %label, ?mode, "no frames, continuing screen-only");
            None
        }
        Err(e) => {
            tracing::warn!("camera unavailable, continuing screen-only: {e:#}");
            None
        }
    }
}

async fn publish(
    tx: &watch::Sender<ipc::Status>,
    tray: &Option<ksni::Handle<tray::Tray>>,
    status: ipc::Status,
) {
    let _ = tx.send(status);
    if let Some(h) = tray {
        h.update(|t| t.status = status).await;
    }
}

async fn run(args: Args) -> Result<()> {
    let dir = args.voice_dir.replacen('~', &std::env::var("HOME")?, 1);
    let t = Instant::now();
    let voice = voice::Voice::new(std::path::Path::new(&dir), args.sid, args.speed)?;
    tracing::info!(ms = t.elapsed().as_millis() as u64, "voice loaded");

    if let Some(text) = args.say {
        voice.say(text);
        wait_ready(&voice, Duration::ZERO).await;
        tokio::time::sleep(Duration::from_millis(400)).await; // player buffer
        return Ok(());
    }

    let ctx = Ctx {
        brain: brain::Brain::new(
            args.api_key
                .clone()
                .context("OPENROUTER_API_KEY is required")?,
            args.model.clone(),
        ),
        voice,
        width: args.width,
        tiles: args.tiles,
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<ipc::Cmd>();
    let mut status = ipc::Status {
        enabled: true,
        auto: args.auto,
        preview: false,
    };
    let (status_tx, status_rx) = watch::channel(status);
    ipc::serve(tx.clone(), status_rx).await?;
    let tray = tray::spawn(tx.clone(), status).await;
    let quit_tx = tx.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = quit_tx.send(ipc::Cmd::Quit);
        // The loop only sees Quit between steps (e.g. not while a camera is still waking up),
        // so a second Ctrl+C force-quits.
        let _ = tokio::signal::ctrl_c().await;
        let _ = std::fs::remove_file(ipc::socket_path());
        std::process::exit(130);
    });

    let mut camera = start_camera(&args).await;
    let mut preview: Option<camera::Preview> = None;
    if args.preview {
        set_preview(&args, &mut preview, &mut status, ipc::Set::On).await;
        publish(&status_tx, &tray, status).await;
    }
    let lookahead = Duration::from_millis(args.lookahead_ms);
    let mut history: Vec<String> = Vec::new();
    let mut queued: Option<ipc::Cmd> = None;
    let mut not_before = Instant::now(); // backoff for continuous mode
    tracing::info!(socket = %ipc::socket_path().display(), auto = status.auto, "ready");

    loop {
        if preview.as_mut().is_some_and(|p| !p.is_running()) {
            preview = None; // the window was closed
            status.preview = false;
            publish(&status_tx, &tray, status).await;
        }
        let cmd = match queued.take() {
            Some(c) => Some(c),
            None => tokio::select! {
                c = rx.recv() => match c { Some(c) => Some(c), None => break },
                _ = tokio::time::sleep(Duration::from_millis(50)) => None,
            },
        };
        let (hint, manual) = match cmd {
            Some(ipc::Cmd::Quit) => break,
            Some(ipc::Cmd::Stop) => {
                ctx.voice.stop();
                continue;
            }
            Some(ipc::Cmd::Auto(set)) => {
                status.auto = set.apply(status.auto);
                publish(&status_tx, &tray, status).await;
                continue;
            }
            Some(ipc::Cmd::Preview(set)) => {
                set_preview(&args, &mut preview, &mut status, set).await;
                publish(&status_tx, &tray, status).await;
                continue;
            }
            Some(ipc::Cmd::Enabled(set)) => {
                status.enabled = set.apply(status.enabled);
                ctx.voice.stop();
                preview = None; // disabling also closes the preview
                status.preview = false;
                camera = None; // stops the pipeline (and the LED) right away
                publish(&status_tx, &tray, status).await;
                if status.enabled {
                    camera = start_camera(&args).await;
                }
                tracing::info!(enabled = status.enabled, "toggled");
                continue;
            }
            Some(ipc::Cmd::Poke(_)) if !status.enabled => continue,
            Some(ipc::Cmd::Poke(hint)) => {
                ctx.voice.stop();
                (hint, true)
            }
            None if status.enabled
                && status.auto
                && Instant::now() >= not_before
                && ctx.voice.ready_within(lookahead) =>
            {
                (None, false)
            }
            None => continue,
        };

        let instruction = match (&hint, manual) {
            (Some(h), _) => format!("The viewer asks for a comment right now. Focus: {h}"),
            (None, true) => {
                "The viewer asks for a comment right now; do not reply SILENCE.".to_string()
            }
            (None, false) => "Narrate now.".to_string(),
        };
        let recent = history[history.len().saturating_sub(6)..].to_vec();
        let outcome = {
            let fut = narrate(&ctx, camera.as_ref(), &recent, &instruction);
            tokio::pin!(fut);
            loop {
                tokio::select! {
                    r = &mut fut => break Some(r),
                    Some(c) = rx.recv() => {
                        if c.cancels_narration() {
                            queued = Some(c);
                            break None; // drops the request in flight
                        }
                        match c {
                            ipc::Cmd::Auto(set) => {
                                status.auto = set.apply(status.auto);
                                publish(&status_tx, &tray, status).await;
                            }
                            ipc::Cmd::Preview(set) => {
                                set_preview(&args, &mut preview, &mut status, set).await;
                                publish(&status_tx, &tray, status).await;
                            }
                            _ => {}
                        }
                    }
                }
            }
        };
        match outcome {
            Some(Ok(Some(line))) => history.push(line),
            Some(Ok(None)) => not_before = Instant::now() + Duration::from_secs(3),
            Some(Err(e)) => {
                tracing::warn!("narration failed: {e:#}");
                not_before = Instant::now() + Duration::from_secs(3);
            }
            None => {} // cancelled by a command, handled next iteration
        }
    }

    ctx.voice.stop();
    let _ = std::fs::remove_file(ipc::socket_path());
    Ok(())
}

/// One look: capture, ask the model, speak clauses as they stream in.
/// Returns the spoken line, or `None` if the model chose silence.
async fn narrate(
    ctx: &Ctx,
    camera: Option<&camera::Camera>,
    recent: &[String],
    instruction: &str,
) -> Result<Option<String>> {
    let t = Instant::now();
    // Capture and encoding are CPU-bound; keep them off the async thread so commands that cancel
    // this narration are still seen promptly.
    let (edge, tiles) = (ctx.width, ctx.tiles);
    let mut images = tokio::task::spawn_blocking(move || {
        let frame = screen::capture().context("screen capture")?;
        screen_tiles(&frame, edge, tiles)
    })
    .await??;
    if let Some(cam) = camera {
        match cam.latest(Duration::from_secs(3)) {
            Some(jpeg) => images.push(("Webcam".to_string(), shrink(&jpeg, 640)?)),
            None => tracing::warn!("no fresh webcam frame, sending screen only"),
        }
    }
    tracing::info!(
        ms = t.elapsed().as_millis() as u64,
        images = images.len(),
        kb = images.iter().map(|(_, j)| j.len()).sum::<usize>() / 1024,
        "captured+encoded"
    );

    let (mut line, mut silent) = (String::new(), false);
    ctx.brain
        .narrate(SYSTEM, recent, &images, instruction, |chunk| {
            if silent || chunk.starts_with("SILENCE") {
                silent = true;
                return;
            }
            println!("{chunk}");
            line.push_str(&chunk);
            line.push(' ');
            ctx.voice.say(chunk);
        })
        .await?;
    Ok((!silent).then(|| line.trim().to_string()))
}

async fn wait_ready(voice: &voice::Voice, lookahead: Duration) {
    while !voice.ready_within(lookahead) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_screens_split_into_readable_tiles() {
        let wide = image::RgbImage::new(5120, 2160);
        let tiles = screen_tiles(&wide, 1568, 0).unwrap();
        assert_eq!(tiles.len(), 2);
        assert!(tiles[0].0.contains("1/2") && tiles[1].0.contains("2/2"));
        let t = image::load_from_memory(&tiles[0].1).unwrap();
        // each 2560x2160 half is limited by its width: 1568/2560 = 0.61x
        assert_eq!((t.width(), t.height()), (1568, 1323));
    }

    #[test]
    fn narrow_screens_stay_one_tile_and_never_upscale() {
        let small = image::RgbImage::new(1280, 720);
        let tiles = screen_tiles(&small, 1568, 0).unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].0, "Screen");
        let t = image::load_from_memory(&tiles[0].1).unwrap();
        assert_eq!((t.width(), t.height()), (1280, 720));
    }

    #[test]
    fn shrink_keeps_aspect_ratio() {
        let src = to_jpeg(&image::RgbImage::new(1920, 1080), 1920).unwrap();
        let out = shrink(&src, 640).unwrap();
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (640, 360));
    }
}
