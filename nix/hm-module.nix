# Home Manager module: `programs.narrator`. Writes ~/.config/narrator/config.toml; the daemon
# reloads it live. The API key is deliberately not an option (it would end up in the store):
# use OPENROUTER_API_KEY, NARRATOR_API_KEY_CMD or ~/.config/narrator/api-key.
self:
{ config, lib, pkgs, ... }:
let
  cfg = config.programs.narrator;
  toml = pkgs.formats.toml { };
in
{
  options.programs.narrator = {
    enable = lib.mkEnableOption "narrator, a live screen and webcam commentator";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.system}.default;
      defaultText = lib.literalExpression "narrator.packages.\${pkgs.system}.default";
      description = "The narrator package to install.";
    };

    settings = lib.mkOption {
      type = toml.type;
      default = { };
      example = lib.literalExpression ''
        {
          trigger = { mode = "timer"; interval_secs = 90; jitter_pct = 25; };
          model.id = "anthropic/claude-haiku-4.5";
          persona = { name = "tyson"; max_words = 20; };
        }
      '';
      description = "Contents of config.toml; see the README for every key. Unset keys use defaults.";
    };
  };

  config = lib.mkIf cfg.enable {
    home.packages = [ cfg.package ];
    xdg.configFile."narrator/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = toml.generate "narrator-config.toml" cfg.settings;
    };
  };
}
