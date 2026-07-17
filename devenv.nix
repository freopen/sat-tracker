{
  pkgs,
  ...
}:

{
  packages = with pkgs; [
    git
    nixfmt
    vim
  ];

  languages = {
    rust.enable = true;
    nix.enable = true;
  };

  enterTest = ''
    echo "Running tests"
    git --version | grep --color=auto "${pkgs.git.version}"
  '';

  git-hooks.hooks = {
    clippy.enable = true;
    deadnix.enable = true;
    nixfmt.enable = true;
    rustfmt.enable = true;
    statix.enable = true;
  };
}
