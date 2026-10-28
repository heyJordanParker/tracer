class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.6.1"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.1/trace-darwin-arm64"
      sha256 "32b629947b548a06b73f8675e975f9feecb7cf8811b6b247c7aab574fa3f2e0f"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.1/trace-darwin-x64"
      sha256 "461fbc624c8831e7b869a0f6e7ccdea564f110adc49cf61917d7d4de3d698070"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.1/trace-linux-arm64"
      sha256 "d6847698f3b103f15e7f8a5a41472e2cb15ec06c25d2344f095747e404c4599e"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.1/trace-linux-x64"
      sha256 "b49bccfd138b45a91b27db182f02a6f48444b367a0ddae332404378207b05cf2"
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
