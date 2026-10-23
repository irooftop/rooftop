#!/usr/bin/env bash
# Renders the Homebrew tap for a published release:
#   Formula/rooftop.rb   the server (built from the source tarball)
#   Casks/irooftop.rb    the desktop app (the macOS DMGs attached to the release)
#
#   scripts/homebrew-tap.sh v0.1.0 ../homebrew-tap
#
# Needs curl and gh (authenticated, or GH_TOKEN). The release workflow runs it on
# every published release and pushes the result to irooftop/homebrew-tap.
set -euo pipefail

tag=${1:?usage: $0 vX.Y.Z TAP_DIR}
out=${2:?usage: $0 vX.Y.Z TAP_DIR}
repo=${GITHUB_REPOSITORY:-irooftop/rooftop}
version=${tag#v}
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "tag must look like vX.Y.Z, got $tag" >&2; exit 1; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sha() { shasum -a 256 "$1" | cut -d' ' -f1; }

curl -fsSL "https://github.com/$repo/archive/refs/tags/$tag.tar.gz" -o "$tmp/src.tar.gz"
src_sha=$(sha "$tmp/src.tar.gz")

# Tauri names the DMGs <productName>_<version>_<aarch64|x64>.dmg.
dmg_sha() {
  local name="iRoof.top_${version}_$1.dmg"
  gh release download "$tag" -R "$repo" -p "$name" -D "$tmp" --clobber
  sha "$tmp/$name"
}
arm_sha=$(dmg_sha aarch64)
intel_sha=$(dmg_sha x64)

mkdir -p "$out/Formula" "$out/Casks"

cat >"$out/Formula/rooftop.rb" <<EOF
class Rooftop < Formula
  desc "Continue a Telegram or WhatsApp chat with an LLM that writes like the person"
  homepage "https://iroof.top/"
  url "https://github.com/$repo/archive/refs/tags/$tag.tar.gz"
  sha256 "$src_sha"
  license "MIT"
  head "https://github.com/$repo.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  service do
    run [opt_bin/"rooftop"]
    environment_variables ROOFTOP_DATA: var/"rooftop"
    working_dir var/"rooftop"
    keep_alive true
    log_path var/"log/rooftop.log"
    error_log_path var/"log/rooftop.log"
  end

  def caveats
    <<~EOS
      Open http://127.0.0.1:8080 after starting it, then set the model in Settings.
      As a background service: brew services start rooftop (data in #{var}/rooftop).
      Optional, to describe GIFs and videos from their frames: brew install ffmpeg
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/rooftop --version")
    port = free_port
    pid = spawn({ "ROOFTOP_DATA" => testpath.to_s }, bin/"rooftop", "--addr", "127.0.0.1:#{port}")
    begin
      sleep 5
      assert_equal "ok", shell_output("curl -fsS http://127.0.0.1:#{port}/healthz")
    ensure
      Process.kill("TERM", pid)
      Process.wait(pid)
    end
  end
end
EOF

cat >"$out/Casks/irooftop.rb" <<EOF
cask "irooftop" do
  arch arm: "aarch64", intel: "x64"

  version "$version"
  sha256 arm:   "$arm_sha",
         intel: "$intel_sha"

  url "https://github.com/$repo/releases/download/v#{version}/iRoof.top_#{version}_#{arch}.dmg"
  name "iRoof.top"
  desc "Talk to the people you miss: continue a chat with an LLM in their style"
  homepage "https://iroof.top/"

  livecheck do
    url :url
    strategy :github_latest
  end

  depends_on :macos

  app "iRoof.top.app"

  zap trash: [
    "~/Library/Application Support/top.iroof.desktop",
    "~/Library/Caches/top.iroof.desktop",
    "~/Library/Preferences/top.iroof.desktop.plist",
    "~/Library/Saved Application State/top.iroof.desktop.savedState",
    "~/Library/WebKit/top.iroof.desktop",
  ]

  caveats <<~EOS
    iRoof.top is not notarized by Apple yet. Open it once, then allow it in
    System Settings → Privacy & Security → Open Anyway. Step by step:
    https://irooftop.github.io/rooftop/guide/unsigned-apps
  EOS
end
EOF

echo "rendered $tag into $out (source $src_sha, arm $arm_sha, intel $intel_sha)"
