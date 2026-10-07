# heckle

A tiny daemon that watches your screen and your face, then says something about it. Out loud.
Think nature documentary, except the wildlife is you at 2 a.m. rewriting the same function.

<p align="center">
  <img width="506" height="460" alt="heckle" src="https://github.com/user-attachments/assets/7f4d10ed-7ea6-4454-bee1-3f613f896cea" />
</p>

> *"Here we see the developer, alone in its natural habitat, opening the same file for the
> fourth time. It will not find what it seeks."*

## How it works

1. **Look.** Grabs your screen (wlr-screencopy, so no portal prompts and no notification
   banners) and, if you allow it, a webcam frame through PipeWire.
2. **Think.** Sends the images to a vision model on [OpenRouter](https://openrouter.ai) with a
   persona and your last few lines, so it doesn't repeat itself. (OpenRouter is there because
   this was hacked together in one evening, not because it's the plan. See
   [Contributing](#contributing).)
3. **Speak.** Streams the answer into a local [Kokoro](https://github.com/k2-fsa/sherpa-onnx)
   voice, sentence by sentence, so it starts talking before the model has finished thinking.
   The voice runs on your machine. For now the pictures do not stay there (see [Privacy](#privacy)).

If nothing interesting happened, the model says nothing. Not everyone gets that kind of restraint.

## Requirements

- Linux on x86_64 with a **wlroots-style Wayland compositor** (niri, sway, Hyprland, ...).
  GNOME and KDE do not offer wlr-screencopy.
- PipeWire (audio out, camera in).
- An [OpenRouter](https://openrouter.ai) API key.
- Nix, if you want the easy route. The package bundles GStreamer, PipeWire's tools and the
  voice model.

## Install

Try it without installing anything:

```sh
nix run github:elsbrock/heckle
```

Or add the flake to your configuration and use the package, the overlay, or the Home Manager
module:

```nix
# flake inputs
heckle.url = "github:elsbrock/heckle";

# Home Manager
imports = [ inputs.heckle.homeManagerModules.default ];
programs.heckle = {
  enable = true;
  settings = {
    trigger = { mode = "timer"; interval_secs = 90; };
    persona.name = "tyson";
  };
};
```

The package also ships a desktop entry (with *Narrate now*, *Stop speaking* and *Toggle on/off*
actions) and an icon, so your launcher will find it.

### The API key

The key never goes in `settings`: that would put it in the world-readable Nix store. heckle looks
for it in this order:

1. `OPENROUTER_API_KEY`
2. `HECKLE_API_KEY_CMD`, a command that prints it, e.g. `op read op://vault/openrouter/key`
3. the file `~/.config/heckle/api-key`

A launcher has no shell environment, so for launcher use pick 2 or 3.

## Use

Start the daemon (`heckle`, or from your launcher). It sits in the tray and waits to be asked.
Drive it from a keybinding with the CLI:

```sh
heckle poke              # comment on what's on screen right now
heckle poke "the tests"  # ...and pay attention to this
heckle stop              # shut up
heckle toggle            # whole daemon on/off (off also releases the camera and its LED)
heckle auto on           # keep commenting (see the trigger modes below)
heckle preview on        # show the webcam feed, to confirm what it sees
heckle status
heckle quit
```

On Wayland an app can't grab global hotkeys, so bind the commands in your compositor. For niri:

```kdl
binds {
    Mod+Shift+H { spawn "heckle" "poke"; }
    Mod+Shift+S { spawn "heckle" "stop"; }
}
```

## Configuration

`~/.config/heckle/config.toml`, every key optional. The daemon notices changes within about a
second, and a file with a typo keeps the last good settings instead of crashing.

```toml
[trigger]
mode = "manual"        # manual | timer | continuous
interval_secs = 60     # timer: quiet time after each line
jitter_pct = 25        # timer: vary the interval so it isn't metronomic

[model]
id = "anthropic/claude-haiku-4.5"   # any vision-capable OpenRouter model

[persona]
name = "attenborough"  # attenborough | tyson | custom
prompt = ""            # the character, when name = "custom"
max_words = 15         # upper bound on the length of a line
```

| mode         | behaviour                                                             |
| ------------ | --------------------------------------------------------------------- |
| `manual`     | Only speaks when poked. The default, and the polite one.              |
| `timer`      | Looks again `interval_secs` (give or take) after each line.           |
| `continuous` | Looks again as soon as the previous line is nearly done. Chatty.      |

Command-line flags (`--model`, `--auto`, `--no-camera`, `--camera-target`, `--sid`, ...) override
the file. `heckle --help` lists them all.

## Privacy

Be clear on what leaves your machine: **every look sends a screenshot (and a webcam frame, unless
you pass `--no-camera`) to OpenRouter and the model provider behind it.** The system prompt tells
the model not to read out passwords, tokens or private messages, but that's a request, not a
guarantee. Don't run it over things you wouldn't show a stranger, and use `heckle toggle` before
you open the password manager. The voice is local and nothing is stored by heckle itself.

## Contributing

**Local models are the big missing piece**, and PRs are very welcome. The model call is one small
module (`src/brain.rs`) that streams chat completions from OpenRouter, so supporting anything that
speaks the OpenAI-style API (Ollama, llama.cpp, LM Studio, vLLM) should mostly be a configurable
base URL plus making the API key optional. A fully offline setup would also fix the
[privacy](#privacy) caveat. Other things that would be nice: more personas, an app blocklist so it
stays quiet around password managers, and compositors beyond wlroots.

Open an issue first if you're planning something big.

## Development

```sh
nix develop
cargo test
cargo run
```

## Status

Works on the author's machine, which is a bold claim in itself. The tray menu is known to be
unreliable on some shells; the CLI and keybindings are the dependable way in.
