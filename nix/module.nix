{ config, lib, pkgs, ... }:
let
  cfg = config.services.fogline;
in
{
  options.services.fogline = {
    enable = lib.mkEnableOption "the Dogpatch e-paper dashboard";
    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = "Runtime path to an environment file containing FOGLINE_ICS.";
    };
    openFirewall = lib.mkEnableOption "LAN access to Fogline on TCP port 8088";
  };

  config = lib.mkIf cfg.enable {
    networking.firewall.allowedTCPPorts = lib.optional cfg.openFirewall 8088;
    users.groups.fogline = { };
    users.users.fogline = {
      isSystemUser = true;
      group = "fogline";
    };
    systemd.services.fogline = {
      description = "Dogpatch e-paper dashboard";
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      wantedBy = [ "multi-user.target" ];
      serviceConfig = {
        ExecStart = "${lib.getExe (pkgs.callPackage ./package.nix { })} serve";
        User = "fogline";
        Group = "fogline";
        StateDirectory = "fogline";
        WorkingDirectory = "/var/lib/fogline";
        Restart = "on-failure";
        RestartSec = 10;
      } // lib.optionalAttrs (cfg.environmentFile != null) {
        EnvironmentFile = cfg.environmentFile;
      };
    };
  };
}
