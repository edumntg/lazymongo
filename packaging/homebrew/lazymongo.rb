# Homebrew formula template for lazymongo.
#
# To publish: create a tap repo (e.g. github.com/<owner>/homebrew-tap),
# copy this file to Formula/lazymongo.rb, and fill in the version and the
# sha256 values from the release assets (each .tar.gz.sha256 file).
# Users then install with:
#   brew install <owner>/tap/lazymongo

class Lazymongo < Formula
  desc "Fast, lightweight terminal UI for MongoDB (lazygit for Mongo)"
  homepage "https://github.com/OWNER/lazymongo"
  version "VERSION"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/OWNER/lazymongo/releases/download/v#{version}/lazymongo-#{version}-aarch64-apple-darwin.tar.gz"
      sha256 "SHA256_MACOS_ARM64"
    end
    on_intel do
      url "https://github.com/OWNER/lazymongo/releases/download/v#{version}/lazymongo-#{version}-x86_64-apple-darwin.tar.gz"
      sha256 "SHA256_MACOS_X86_64"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/OWNER/lazymongo/releases/download/v#{version}/lazymongo-#{version}-aarch64-unknown-linux-musl.tar.gz"
      sha256 "SHA256_LINUX_ARM64"
    end
    on_intel do
      url "https://github.com/OWNER/lazymongo/releases/download/v#{version}/lazymongo-#{version}-x86_64-unknown-linux-musl.tar.gz"
      sha256 "SHA256_LINUX_X86_64"
    end
  end

  def install
    bin.install "lazymongo"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/lazymongo --version")
  end
end
