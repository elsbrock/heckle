//! Streaming chat completions against OpenRouter, yielding whole sentences as they complete.

use std::time::Instant;

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde_json::{Value, json};

pub struct Brain {
    http: reqwest::Client,
    key: String,
    pub model: String,
}

impl Brain {
    pub fn new(key: String, model: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            key,
            model,
        }
    }

    /// Stream a narration for the given JPEG frames; `on_chunk` fires per completed sentence.
    pub async fn narrate(
        &self,
        system: &str,
        history: &[String],
        jpegs: &[Vec<u8>],
        mut on_chunk: impl FnMut(String),
    ) -> Result<()> {
        let start = Instant::now();
        let mut content: Vec<Value> = jpegs
            .iter()
            .map(|j| json!({"type": "image_url", "image_url": {"url": format!("data:image/jpeg;base64,{}", STANDARD.encode(j))}}))
            .collect();
        content.push(json!({"type": "text", "text": format!(
            "Your previous lines (do not repeat):\n{}\nNarrate now.", history.join("\n"))}));

        let resp = self
            .http
            .post("https://openrouter.ai/api/v1/chat/completions")
            .bearer_auth(&self.key)
            .json(&json!({
                "model": self.model,
                "max_tokens": 80,
                "stream": true,
                "provider": {"sort": "latency"},
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": content},
                ],
            }))
            .send()
            .await?;
        if !resp.status().is_success() {
            bail!(
                "openrouter {}: {}",
                resp.status(),
                resp.text().await.unwrap_or_default()
            );
        }
        tracing::info!(ms = start.elapsed().as_millis() as u64, "response headers");

        let mut stream = resp.bytes_stream();
        let (mut raw, mut pending) = (Vec::new(), String::new());
        let mut first_token = true;
        while let Some(chunk) = stream.next().await {
            raw.extend_from_slice(&chunk.context("stream")?);
            while let Some(pos) = raw.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = raw.drain(..=pos).collect();
                let line = String::from_utf8_lossy(&line);
                let Some(data) = line.trim().strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    break;
                }
                let Ok(v) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                let Some(delta) = v["choices"][0]["delta"]["content"].as_str() else {
                    continue;
                };
                if first_token && !delta.is_empty() {
                    tracing::info!(ms = start.elapsed().as_millis() as u64, "first token");
                    first_token = false;
                }
                pending.push_str(delta);
                while let Some(end) = chunk_end(&pending) {
                    let s: String = pending.drain(..end).collect();
                    on_chunk(s.trim().to_string());
                }
            }
        }
        if !pending.trim().is_empty() {
            on_chunk(pending.trim().to_string());
        }
        tracing::info!(ms = start.elapsed().as_millis() as u64, "stream done");
        Ok(())
    }
}

/// Byte index just past the first sentence end, or past a clause break (`, ; : —`) once at least
/// `MIN_CLAUSE` bytes are buffered, so speech can start before the sentence is finished.
fn chunk_end(s: &str) -> Option<usize> {
    const MIN_CLAUSE: usize = 25;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let spaced = it.peek().is_some_and(|&(_, n)| n.is_whitespace());
        if spaced
            && (matches!(c, '.' | '!' | '?')
                || (i >= MIN_CLAUSE && matches!(c, ',' | ';' | ':' | '—')))
        {
            return Some(i + c.len_utf8());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::chunk_end;

    #[test]
    fn splits_on_sentence_end() {
        assert_eq!(chunk_end("Hello there. More"), Some(12));
        assert_eq!(chunk_end("No end yet"), None);
        assert_eq!(chunk_end("Ends at tail."), None); // wait for whitespace or flush
    }

    #[test]
    fn splits_on_clause_only_when_long_enough() {
        assert_eq!(chunk_end("Well, hmm"), None);
        let s = "The specimen hunches closer to the terminal, squinting";
        assert_eq!(chunk_end(s), Some(44));
    }
}
