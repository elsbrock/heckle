mod brain;
mod screen;
mod voice;

use std::io::Cursor;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use image::{ImageFormat, imageops::FilterType};

const SYSTEM: &str = "You are Sir David Attenborough narrating a nature documentary. The subject is a human \
at its workstation (the screen). Observe behaviour and what is on screen with hushed wonder and dry British wit. \
Speak in ONE sentence of at most 15 words, present tense, as spoken narration: no markdown, no emoji. \
Be specific to what you see and never repeat your earlier lines. \
Never read out passwords, tokens or private message contents. \
If nothing has meaningfully changed, reply exactly: SILENCE";

#[derive(Parser)]
struct Args {
    #[arg(long, env = "OPENROUTER_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    #[arg(long, default_value = "anthropic/claude-haiku-4.5")]
    model: String,
    /// Longest edge of the screen frame sent to the model.
    #[arg(long, default_value_t = 768)]
    width: u32,
    /// Stop after N narrations (0 = run until interrupted).
    #[arg(long, default_value_t = 0)]
    repeat: u32,
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
    let scale = edge as f32 / w.max(h) as f32;
    let small = image::imageops::resize(
        img,
        (w as f32 * scale) as u32,
        (h as f32 * scale) as u32,
        FilterType::Triangle,
    );
    let mut out = Cursor::new(Vec::new());
    small.write_to(&mut out, ImageFormat::Jpeg)?;
    Ok(out.into_inner())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "narrator=info".into()),
        )
        .init();
    let args = Args::parse();

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

    let brain = brain::Brain::new(
        args.api_key.context("OPENROUTER_API_KEY is required")?,
        args.model,
    );
    let lookahead = Duration::from_millis(args.lookahead_ms);
    let mut history: Vec<String> = Vec::new();
    let mut n = 0;
    while args.repeat == 0 || n < args.repeat {
        wait_ready(&voice, lookahead).await;
        let t = Instant::now();
        let frame = screen::capture().context("screen capture")?;
        let jpeg = to_jpeg(&frame, args.width)?;
        tracing::info!(ms = t.elapsed().as_millis() as u64, "captured+encoded");

        let recent = &history[history.len().saturating_sub(6)..];
        let (mut line, mut silent) = (String::new(), false);
        let res = brain
            .narrate(SYSTEM, recent, &[jpeg], |chunk| {
                if silent || chunk.starts_with("SILENCE") {
                    silent = true;
                    return;
                }
                println!("{chunk}");
                line.push_str(&chunk);
                line.push(' ');
                voice.say(chunk);
            })
            .await;
        if let Err(e) = res {
            tracing::warn!("narration failed: {e:#}");
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        if silent {
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }
        history.push(line.trim().to_string());
        n += 1;
    }
    wait_ready(&voice, Duration::ZERO).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    Ok(())
}

async fn wait_ready(voice: &voice::Voice, lookahead: Duration) {
    while !voice.ready_within(lookahead) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
