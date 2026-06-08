{ inputs, ... }:
{
  perSystem =
    {
      pkgs,
      lib,
      system,
      ...
    }:
    let
      mkFlint = import ../toolchain.nix { inherit inputs; };
      flint-editor = mkFlint pkgs;
    in
    {
      packages = {
        default = flint-editor;
        debug = flint-editor.override { profile = "dev"; };
      };
    }
    // lib.optionalAttrs (lib.hasSuffix "linux" system) {
      checks.a11y-test = import ../tests/a11y.nix {
        inherit pkgs inputs;
      };
    };
}
