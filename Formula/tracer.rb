class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.3.0"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.3.0/trace-darwin-arm64"
      sha256 "e3b2455bd80d57917659f1ff068f574751ad81decbd29654fd2bdb164cbca2ae"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.3.0/trace-darwin-x64"
      sha256 "7c04303d2b30d0a390a74e956e3e1691f089dc3fd12658dcf3945e96b347376d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.3.0/trace-linux-arm64"
      sha256 "cd98882bf86aa020415ba0be1194801a1d6d99002c94a0c4839f8fe0fd96f9c0"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.3.0/trace-linux-x64"
      sha256 "c8e43fb0253af31ea02bf3d13cd44f48bb9df0b3ca91d551a6dd445af8410860"
    end
  end

  def install
    bin.install Dir["trace-*"].fetch(0) => "trace"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/trace --version")
    system "git", "init", "--quiet"
    (testpath/"app.py").write "def total(items):\n    return sum(items)\n"
    assert_match "def total(items)", shell_output("#{bin}/trace structure app.py")
  end
end
