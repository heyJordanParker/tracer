class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.8.1"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.1/trace-darwin-arm64"
      sha256 "69a4e652cf4fde033c3108c64f8367d28289cfdfde2ea100b27e2c70fe96eb38"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.1/trace-darwin-x64"
      sha256 "87806eae4a3605aefce609be6f085eda6ed99c5b177d1fc1705d16ef5289e137"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.1/trace-linux-arm64"
      sha256 "c120e1f6f0cfb708ce97d1e2889cb8a43383656d7f5d8bd4a8f80030d4363239"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.1/trace-linux-x64"
      sha256 "c517772386f5e1842cfe738f1fa8fda0bb8a5c50423e5ede49410ed570aa0808"
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
