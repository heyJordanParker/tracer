class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.9.0"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.9.0/trace-darwin-arm64"
      sha256 "dd0489206f688e6c387c97b5c41888210229fa026d1b5fe1cb04bfe9f2f30acb"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.9.0/trace-darwin-x64"
      sha256 "18f37c91bd2d7aee70ee9057bea28b381c38b3419b9b120aa1ec54adedde3648"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.9.0/trace-linux-arm64"
      sha256 "f56e514ecfd920ff4054986fced5a5496b9e4a47833758f2182b954702aad9e5"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.9.0/trace-linux-x64"
      sha256 "6b28458cc5d7145fb56fcc037c1fc8b00596bd3668d57339e5f20e1675491664"
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
