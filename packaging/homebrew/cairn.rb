# typed: false
# frozen_string_literal: true

# Cairn — harness-agnostic agent memory framework.
#
# Distribution path: brief §16. v0.1 ships a single Rust binary built from
# source via `cargo install`. Bottles are deferred to #141.
class Cairn < Formula
  desc "Harness-agnostic agent memory: one binary, one SQLite file, one vault"
  homepage "https://github.com/windoliver/cairn"
  # First stable release block. URL and sha256 are populated by release #141
  # when the v0.1.0 tag is cut. Until then `brew install --HEAD cairn` is the
  # only supported install path; the stable block is present so `brew audit`
  # has a release URL to validate.
  url "https://github.com/windoliver/cairn/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license "Apache-2.0"
  head "https://github.com/windoliver/cairn.git", branch: "main"

  depends_on "rust" => :build

  def install
    # Builds the `cairn` binary out of crates/cairn-cli. std_cargo_args
    # already injects `--locked` and `--root=#{prefix}`; we only need to
    # add `--no-track` to keep `cargo install` from leaving a metadata
    # file in the build sandbox.
    system "cargo", "install", *std_cargo_args(path: "crates/cairn-cli"), "--no-track"
  end

  test do
    # Brew's own smoke: bin runs, vault bootstraps, status returns a
    # well-formed envelope. Mirrors scripts/install-smoke.sh in miniature.
    assert_match(/cairn /, shell_output("#{bin}/cairn --version"))
    ENV["CAIRN_VAULT"] = testpath.to_s
    (testpath/".cairn").mkpath
    (testpath/".cairn/config.yaml").write("search:\n  local_embeddings: false\n")
    system bin/"cairn", "bootstrap", "--vault-path", testpath, "--json"
    status = shell_output("#{bin}/cairn status --json")
    assert_match(/"capabilities"/, status)
  end
end
