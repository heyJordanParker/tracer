class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.2.0"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.2.0/trace-darwin-arm64"
      sha256 "dd41dd664566ac4d995158f55cc3d6b89b980e63f335b2f585d5398b0beedf73"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.2.0/trace-darwin-x64"
      sha256 "62edf8fe6b8a8615f013843b6534b82ed4dac8354529a669a8bf915289bb60d1"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.2.0/trace-linux-arm64"
      sha256 "82286f05a0495c0ace6b41d5aeee231debe21ae13fa8e6e3bec8facf4a1e987e"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.2.0/trace-linux-x64"
      sha256 "e8305b08d42be352f4d13c3b2c98dfc9a8ca10110d167d6fdbd1332fccfb02a1"
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
