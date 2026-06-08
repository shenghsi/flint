{ inputs, ... }:
{
  flake.overlays.default =
    final: _:
    let
      mkFlint = import ../toolchain.nix { inherit inputs; };
    in
    {
      flint-editor = mkFlint final;
    };
}
