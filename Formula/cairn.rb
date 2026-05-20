# typed: false
# frozen_string_literal: true

# Cairn — harness-agnostic agent memory framework.
#
# Distribution path: brief §16. v0.1 ships a single Rust binary built from
# source via `cargo install`. Bottles are deferred to #141.
class Cairn < Formula
  desc "Harness-agnostic agent memory: one binary, one SQLite file, one vault"
  homepage "https://github.com/windoliver/cairn"
  license "Apache-2.0"
  # HEAD-only until v0.1.0 ships. The stable `url`/`sha256` pair will be
  # added by release #141 with a real tarball digest; advertising it now
  # with a placeholder sha256 would make `brew install cairn` (the default
  # stable path) fail at checksum verification. Pre-release users install
  # with `brew install --HEAD cairn`.
  head "https://github.com/windoliver/cairn.git", branch: "main"

  depends_on "rust" => :build

  def install
    # Builds the user-facing `cairn` binary out of crates/cairn-cli.
    # `--bin=cairn` excludes the internal `cairn-docgen` doc generator,
    # which is a build-time tool, not part of the supported CLI surface.
    # std_cargo_args already injects `--locked` and `--root=#{prefix}`;
    # we only need to add `--no-track` to keep `cargo install` from
    # leaving a metadata file in the build sandbox.
    system "cargo", "install", *std_cargo_args(path: "crates/cairn-cli"),
           "--bin=cairn", "--no-track"
  end

  test do
    # Brew's own smoke: bin runs, vault bootstraps, status returns a
    # well-formed envelope. Mirrors scripts/install-smoke.sh in miniature.
    assert_match(/cairn /, shell_output("#{bin}/cairn --version"))
    # Single-binary contract: only `cairn` ships; the internal docgen
    # tool must not be installed.
    refute_path_exists bin/"cairn-docgen"
    ENV["CAIRN_VAULT"] = testpath.to_s
    (testpath/".cairn").mkpath
    (testpath/".cairn/config.yaml").write("search:\n  local_embeddings: false\n")
    system bin/"cairn", "bootstrap", "--vault-path", testpath, "--json"
    status = shell_output("#{bin}/cairn status --json")
    assert_match(/"capabilities"/, status)
  end
end
