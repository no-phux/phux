# Small, temporary bridges for tools that lag behind the repository pins in
# nixpkgs. Prefer nixpkgs again as soon as its version matches exactly.
{
  pkgs,
  miseTools,
}:
let
  inherit (pkgs.stdenv.hostPlatform) system;
  webManifest = builtins.fromTOML (builtins.readFile ../clients/phux-web/Cargo.toml);
  vtWebManifest = builtins.fromTOML (builtins.readFile ../clients/phux-vt-web/Cargo.toml);
  wasmBindgenPin = webManifest.dependencies.wasm-bindgen;
  wasmBindgenVersion =
    assert pkgs.lib.hasPrefix "=" wasmBindgenPin;
    assert vtWebManifest.dependencies.wasm-bindgen == wasmBindgenPin;
    pkgs.lib.removePrefix "=" wasmBindgenPin;
  targets = {
    aarch64-darwin = "aarch64-apple-darwin";
    x86_64-darwin = "x86_64-apple-darwin";
    aarch64-linux = "aarch64-unknown-linux-musl";
    x86_64-linux = "x86_64-unknown-linux-musl";
  };
  # SHA-256 digests verified against downloaded bytes and upstream metadata:
  # https://api.github.com/repos/casey/just/releases/tags/1.59.0
  # https://api.github.com/repos/wasm-bindgen/wasm-bindgen/releases/tags/0.2.130
  # Linux assets are static musl binaries; no loader patching is needed.
  justDigests = {
    "1.59.0" = {
      aarch64-darwin = "sha256-ZXTOp3TtTK4S4ktqoRhX3jPoDatGZ89ij9WEDflqKp0=";
      x86_64-darwin = "sha256-A26tnfm8MduumTSuO0I27RJNC6/P/TNDlGCuCE1cnLc=";
      aarch64-linux = "sha256-I4cbFwK/1VD7X5AXeP1CkOzy7gNKRaiJ++ltibGICG0=";
      x86_64-linux = "sha256-QJBEYXtvBlF8EDXlXKOMjIVv+LZQ5BvCqh74LVkDoj4=";
    };
  };
  wasmBindgenDigests = {
    "0.2.130" = {
      aarch64-darwin = "sha256-uQ3bk1f1hLGviX1YrXfkzOK0j8F+KpZ7wkmof3KAlEQ=";
      x86_64-darwin = "sha256-6yCcEVt8uVgyQjwao/TQbWxT9j73T8S2+3nvCy7qXxA=";
      aarch64-linux = "sha256-mf/JPOgAgbkWMuK1ObeCghqPNDmrD93GRSh0YrL1XT0=";
      x86_64-linux = "sha256-lofzu1Ubn2bzftMqQNeROgD3li22UwVOVoV7wwbDIn8=";
    };
  };
  # Browser tooling follows the latest stable wasm-pack independently of the
  # exact wasm-bindgen schema pin above. Official Linux assets are static musl.
  # All hashes below match downloaded bytes and GitHub release asset digests:
  # https://api.github.com/repos/wasm-bindgen/wasm-pack/releases/tags/v0.15.1
  # https://api.github.com/repos/EmbarkStudios/cargo-deny/releases/tags/0.20.2
  wasmPackVersion = "0.15.1";
  wasmPackDigests = {
    "0.15.1" = {
      aarch64-darwin = "sha256-vK8qQYrjnIdLBy6hIv7FjhGXCGLL2P1Oyi8AoX92xTY=";
      x86_64-darwin = "sha256-k4dxWtNFof5YdR1gyA9J2ROZV1QHKLr9BmjQBaN872I=";
      aarch64-linux = "sha256-cASFPHklrhmEAztfJYZuxgS7ySHvlW1ebFWVvFr20ww=";
      x86_64-linux = "sha256-QQEBAJc5udAcfVr/qq1unJoktMumeRpIBduOibm/evI=";
    };
  };
  # Only maintained Intel Darwin lags; the other three use nixpkgs unchanged.
  cargoDenyDigests = {
    "0.20.2" = {
      x86_64-darwin = "sha256-JI2n9YFyTkcAcZkMCI/8VcgRmBcV9MvbJYYh+3n4t6Y=";
    };
  };
  # SHA-256 of https://www.python.org/ftp/python/3.15.2/Python-3.15.2.tar.xz
  # verified against downloaded bytes (not an unpacked hash).
  pythonDigests = {
    "3.15.2" = "sha256-i1U20oAULs9s3+FdEqIbocZeMAu/eyio0M24SVdXl94=";
  };
in
{
  just =
    if pkgs.just.version == miseTools.just then
      pkgs.just
    else
      pkgs.stdenvNoCC.mkDerivation {
        pname = "just";
        version = miseTools.just;
        src = pkgs.fetchurl {
          url = "https://github.com/casey/just/releases/download/${miseTools.just}/just-${miseTools.just}-${targets.${system}}.tar.gz";
          hash =
            (justDigests.${miseTools.just} or (throw "No just digests pinned for ${miseTools.just}")).${system};
        };
        sourceRoot = ".";
        dontConfigure = true;
        dontBuild = true;
        nativeBuildInputs = [ pkgs.installShellFiles ];
        installPhase = ''
          runHook preInstall
          install -Dm755 just $out/bin/just
          installManPage just.1
          installShellCompletion --cmd just --bash completions/just.bash --fish completions/just.fish --zsh completions/just.zsh
          runHook postInstall
        '';
        meta = pkgs.just.meta // {
          platforms = builtins.attrNames targets;
        };
      };

  wasm-bindgen-cli =
    if pkgs.wasm-bindgen-cli.version == wasmBindgenVersion then
      pkgs.wasm-bindgen-cli
    else
      pkgs.stdenvNoCC.mkDerivation {
        pname = "wasm-bindgen-cli";
        version = wasmBindgenVersion;
        src = pkgs.fetchurl {
          url = "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${wasmBindgenVersion}/wasm-bindgen-${wasmBindgenVersion}-${targets.${system}}.tar.gz";
          hash =
            (wasmBindgenDigests.${wasmBindgenVersion}
              or (throw "No wasm-bindgen-cli digests pinned for ${wasmBindgenVersion}")
            ).${system};
        };
        dontConfigure = true;
        dontBuild = true;
        installPhase = ''
          runHook preInstall
          mkdir -p $out/bin
          install -m755 wasm-bindgen wasm-bindgen-test-runner wasm2es6js $out/bin/
          runHook postInstall
        '';
        meta = pkgs.wasm-bindgen-cli.meta // {
          platforms = builtins.attrNames targets;
        };
      };

  cargo-deny =
    if pkgs.cargo-deny.version == miseTools.cargo-deny then
      pkgs.cargo-deny
    else
      pkgs.stdenvNoCC.mkDerivation {
        pname = "cargo-deny";
        version = miseTools.cargo-deny;
        src = pkgs.fetchurl {
          url = "https://github.com/EmbarkStudios/cargo-deny/releases/download/${miseTools.cargo-deny}/cargo-deny-${miseTools.cargo-deny}-${targets.${system}}.tar.gz";
          hash =
            (cargoDenyDigests.${miseTools.cargo-deny}
              or (throw "No cargo-deny digests pinned for ${miseTools.cargo-deny}")
            ).${system} or (throw "No cargo-deny release digest pinned for ${system}");
        };
        dontConfigure = true;
        dontBuild = true;
        installPhase = ''
          runHook preInstall
          install -Dm755 cargo-deny $out/bin/cargo-deny
          runHook postInstall
        '';
        meta = pkgs.cargo-deny.meta;
      };

  wasm-pack =
    if pkgs.wasm-pack.version == wasmPackVersion then
      pkgs.wasm-pack
    else
      pkgs.stdenvNoCC.mkDerivation {
        pname = "wasm-pack";
        version = wasmPackVersion;
        src = pkgs.fetchurl {
          url = "https://github.com/wasm-bindgen/wasm-pack/releases/download/v${wasmPackVersion}/wasm-pack-v${wasmPackVersion}-${targets.${system}}.tar.gz";
          hash = wasmPackDigests.${wasmPackVersion}.${system};
        };
        dontConfigure = true;
        dontBuild = true;
        installPhase = ''
          runHook preInstall
          install -Dm755 wasm-pack $out/bin/wasm-pack
          runHook postInstall
        '';
        meta = pkgs.wasm-pack.meta // {
          platforms = builtins.attrNames targets;
        };
      };

  python =
    if pkgs.python315.version == miseTools.python then
      pkgs.python315
    else
      # Override the interpreter arguments, not just src/version: Python's
      # package set, sysconfig and spliced interpreters must agree as well.
      (pkgs.extend (
        final: prev: {
          python315 =
            (prev.python315.override {
              self = final.python315;
              sourceVersion =
                let
                  parts = pkgs.lib.splitString "." miseTools.python;
                in
                {
                  major = builtins.elemAt parts 0;
                  minor = builtins.elemAt parts 1;
                  patch = builtins.elemAt parts 2;
                  suffix = "";
                };
              hash =
                pythonDigests.${miseTools.python}
                  or (throw "No Python source digest pinned for ${miseTools.python}");
            }).overrideAttrs
              (old: {
                # 3.15.2 guards knownfiles on Windows, so nixpkgs' unindented
                # mimetypes.patch no longer applies. Preserve both that security fix
                # and Nix's hermetic mailcap path until nixpkgs catches up.
                patches = builtins.filter (p: builtins.baseNameOf p != "mimetypes.patch") old.patches ++ [
                  (pkgs.writeText "python315-mimetypes.patch" ''
                    --- a/Lib/mimetypes.py
                    +++ b/Lib/mimetypes.py
                    @@ -48,17 +48,7 @@
                         # Only the registry is used by default.
                         knownfiles = []
                     else:
                    -    knownfiles = [
                    -        "/etc/mime.types",
                    -        "/etc/httpd/mime.types",                    # Mac OS X
                    -        "/etc/httpd/conf/mime.types",               # Apache
                    -        "/etc/apache/mime.types",                   # Apache 1
                    -        "/etc/apache2/mime.types",                  # Apache 2
                    -        "/usr/local/etc/httpd/conf/mime.types",
                    -        "/usr/local/lib/netscape/mime.types",
                    -        "/usr/local/etc/httpd/conf/mime.types",     # Apache 1.2
                    -        "/usr/local/etc/mime.types",                # Apache 1.3
                    -        ]
                    +    knownfiles = ["@mime-types@/etc/mime.types"]
                    ${" "}
                     inited = False
                     _db = None
                  '')
                ];
              });
        }
      )).python315;
}
