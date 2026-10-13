class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.5.0"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.5.0/trace-darwin-arm64"
      sha256 "0197991abe0eef9b8254121f27a6a26774c25dfb3d8253f8598d9fac5db66fa4"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.5.0/trace-darwin-x64"
      sha256 "33e5e2e00e71607c5ab9efa1243491288745df667195767513a34c04e16f251e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.5.0/trace-linux-arm64"
      sha256 "361256d1f3e64a5cbe77ecc75ca7c892c9601117c4d426e7aa0a1398c2583983"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.5.0/trace-linux-x64"
      sha256 "58c1411013c937749aa4b269a572116a93298cdbb1b2120e5f4a1d9fc21c1adf"
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
