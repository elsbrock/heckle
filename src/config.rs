//! `~/.config/narrator/config.toml`: every field is optional, and a missing file means defaults.
//! The daemon re-reads it when it changes, so edits apply without a restart.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// What makes the narrator speak.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Trigger {
    /// Only when asked (`narrator poke`, a hotkey, or the tray).
    #[default]
    Manual,
    /// Every `interval_secs` (with jitter), measured from the end of the previous line.
    Timer,
    /// Look again as soon as the previous line is nearly finished.
    Continuous,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TriggerConfig {
    pub mode: Trigger,
    /// Timer mode: seconds of quiet after a line before the next look.
    pub interval_secs: u64,
    /// Timer mode: vary the interval by up to this many percent, so it isn't metronomic.
    pub jitter_pct: u64,
}

impl Default for TriggerConfig {
    fn default() -> Self {
        Self {
            mode: Trigger::Manual,
            interval_secs: 60,
            jitter_pct: 25,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    /// Any vision-capable OpenRouter model id.
    pub id: String,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            id: "anthropic/claude-haiku-4.5".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Persona {
    #[default]
    Attenborough,
    Tyson,
    /// Use `[persona] prompt` as the character description.
    Custom,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PersonaConfig {
    pub name: Persona,
    /// Character description when `name = "custom"`.
    pub prompt: String,
    /// Upper bound on the length of a line.
    pub max_words: u32,
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            name: Persona::Attenborough,
            prompt: String::new(),
            max_words: 15,
        }
    }
}

const ATTENBOROUGH: &str = "You are Sir David Attenborough narrating a nature documentary. \
    The subject is a human at its workstation. You get the screen and, when available, a webcam \
    image of the human. Observe their behaviour, mood and what is on screen with hushed wonder \
    and dry British wit.";

const TYSON: &str = "You are Neil deGrasse Tyson. The subject is a human at its workstation. \
    You get the screen and, when available, a webcam image of the human. React with infectious \
    awe, tying what you see to cosmic scale, physics and deep time.";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub trigger: TriggerConfig,
    pub model: ModelConfig,
    pub persona: PersonaConfig,
}

impl Config {
    /// The full system prompt: the character, then the rules every character shares.
    pub fn system_prompt(&self) -> String {
        let p = &self.persona;
        let character = match p.name {
            Persona::Attenborough => ATTENBOROUGH,
            Persona::Tyson => TYSON,
            // An empty custom prompt falls back to the default character.
            Persona::Custom if p.prompt.trim().is_empty() => ATTENBOROUGH,
            Persona::Custom => p.prompt.trim(),
        };
        format!(
            "{character} Speak in ONE sentence of at most {} words, present tense, as spoken \
             narration: no markdown, no emoji. Be specific to what you see and never repeat your \
             earlier lines. Never read out passwords, tokens or private message contents. \
             If nothing has meaningfully changed, reply exactly: SILENCE",
            p.max_words.max(3)
        )
    }
}

pub fn path() -> Result<PathBuf> {
    let dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var("HOME").context("HOME is not set")?).join(".config"),
    };
    Ok(dir.join("narrator/config.toml"))
}

/// Read the config; a missing file is not an error.
pub fn load(path: &std::path::Path) -> Result<Config> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_gives_defaults() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c, Config::default());
        assert_eq!(c.trigger.mode, Trigger::Manual);
        assert_eq!(c.model.id, "anthropic/claude-haiku-4.5");
    }

    #[test]
    fn partial_file_overrides_only_what_it_names() {
        let c: Config = toml::from_str(
            "[trigger]\nmode = \"timer\"\ninterval_secs = 90\n\
             [model]\nid = \"google/gemini-2.5-flash\"\n",
        )
        .unwrap();
        assert_eq!(c.trigger.mode, Trigger::Timer);
        assert_eq!(c.trigger.interval_secs, 90);
        assert_eq!(c.trigger.jitter_pct, 25); // untouched default
        assert_eq!(c.model.id, "google/gemini-2.5-flash");
        assert_eq!(c.persona, PersonaConfig::default());
    }

    #[test]
    fn unknown_mode_is_an_error() {
        assert!(toml::from_str::<Config>("[trigger]\nmode = \"sometimes\"\n").is_err());
    }

    #[test]
    fn prompts_follow_the_persona() {
        let mut c = Config::default();
        assert!(c.system_prompt().contains("Attenborough"));
        assert!(c.system_prompt().contains("at most 15 words"));
        c.persona.name = Persona::Tyson;
        assert!(c.system_prompt().contains("Tyson"));
        c.persona.name = Persona::Custom;
        c.persona.prompt = "You are a grumpy sports commentator.".to_string();
        c.persona.max_words = 10;
        let p = c.system_prompt();
        assert!(p.starts_with("You are a grumpy sports commentator."));
        assert!(p.contains("at most 10 words") && p.contains("SILENCE"));
    }

    #[test]
    fn empty_custom_prompt_falls_back_to_the_default_character() {
        let mut c = Config::default();
        c.persona.name = Persona::Custom;
        assert!(c.system_prompt().contains("Attenborough"));
    }
}
