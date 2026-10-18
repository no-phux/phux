# Darwin SDK support for the development shell. Does not change SDK selection.
{ pkgs }:
{
  # Archive tools include the nmedit invoked by the Ghostty source build.
  packages = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.cctools ];

  # The wrapped cc/ld derive both -isysroot and -syslibroot from
  # DEVELOPER_DIR (nixpkgs' darwin-sdk-setup.bash), so this shell keeps
  # nixpkgs' default: its own Apple SDK. It used to adopt a host Xcode
  # here so Ghostty's build could find `xcrun nmedit`; that swapped the
  # linker's SDK under nixpkgs' ld64, which predates the `arm64e.x1`
  # TBD targets Xcode 27 ships, and every link in the shell failed.
  # `cctools` above provides nmedit, and xcrun resolves both the SDK
  # and nmedit under this DEVELOPER_DIR. The probe below only reports
  # a host where that resolution is broken (no Command Line Tools).
  shellHook = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isDarwin ''
    if ! command -v xcrun >/dev/null 2>&1 ||
       ! xcrun --show-sdk-path >/dev/null 2>&1 ||
       ! xcrun --find nmedit >/dev/null 2>&1; then
      echo "phux: warning: xcrun cannot resolve the SDK or nmedit under" >&2
      echo "  DEVELOPER_DIR=$DEVELOPER_DIR; libghostty-vt-sys's Darwin build" >&2
      echo "  will likely fail. Install Apple's Command Line Tools; see" >&2
      echo "  docs/development.md#platform-packages." >&2
    fi
  '';
}
