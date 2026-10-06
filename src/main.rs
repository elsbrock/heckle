mod brain;
mod screen;
mod voice;

use std::io::Cursor;
use std::time::Instant;

use anyhow::{Context, Result};
use clap::Parser;
use image::{ImageFormat, imageops::FilterType};

const SYSTEM: &str = "You are Sir David Attenborough narrating a nature documentary. The subject is a human \
at its workstation (the screen). Observe behaviour and what is on screen with hushed wonder and dry British wit. \
Speak in ONE short sentence, present tense, as spoken narration: no markdown, no emoji. \
Never read out passwords, tokens or private message contents.";

#[derive(Parser)]
struct Args {
    #[arg(long, env = "OPENROUTER_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    #[arg(long, default_value = "anthropic/claude-haiku-4.5")]
    model: String,
    /// Longest edge of the screen frame sent to the model.
    #[arg(long, default_value_t = 768)]
    width: u32,
    /// Send N requests on one HTTP client (one capture each) to measure warm-connection latency.
    #[arg(long, default_value_t = 1)]
    repeat: u32,
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

    if let Some(text) = &args.say {
        let dir = args.voice_dir.replacen('~', &std::env::var("HOME")?, 1);
        let t = Instant::now();
        let voice = voice::Voice::new(std::path::Path::new(&dir), args.sid, args.speed)?;
        tracing::info!(ms = t.elapsed().as_millis() as u64, "voice loaded");
        voice.speak(text)?;
        // Give the player time to drain before exiting (spike only).
        tokio::time::sleep(std::time::Duration::from_secs(8)).await;
        return Ok(());
    }

    let brain = brain::Brain::new(
        args.api_key.context("OPENROUTER_API_KEY is required")?,
        args.model,
    );
    for i in 0..args.repeat {
        tracing::info!(run = i + 1, "---");
        let t = Instant::now();
        let frame = screen::capture().context("screen capture")?;
        tracing::info!(
            ms = t.elapsed().as_millis() as u64,
            w = frame.width(),
            h = frame.height(),
            "captured"
        );
        let jpeg = to_jpeg(&frame, args.width)?;
        tracing::info!(
            ms = t.elapsed().as_millis() as u64,
            bytes = jpeg.len(),
            "encoded"
        );
        brain
            .narrate(SYSTEM, &[], &[jpeg], |s| println!("{s}"))
            .await?;
    }
    Ok(())
}
