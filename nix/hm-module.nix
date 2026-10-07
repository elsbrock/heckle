# Home Manager module: `programs.heckle`. Writes ~/.config/heckle/config.toml; the daemon
# reloads it live. The API key is deliberately not an option (it would end up in the store):
# use OPENROUTER_API_KEY, HECKLE_API_KEY_CMD or ~/.config/heckle/api-key.
self:
{ config, lib, pkgs, ... }:
let
  cfg = config.programs.heckle;
  toml = pkgs.formats.toml { };
in
{
  options.programs.heckle = {
    enable = lib.mkEnableOption "heckle, a live screen and webcam commentator";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.system}.default;
      defaultText = lib.literalExpression "heckle.packages.\${pkgs.system}.default";
      description = "The heckle package to install.";
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
    xdg.configFile."heckle/config.toml" = lib.mkIf (cfg.settings != { }) {
      source = toml.generate "heckle-config.toml" cfg.settings;
    };
  };
}
