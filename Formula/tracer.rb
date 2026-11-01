class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.8.0"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.0/trace-darwin-arm64"
      sha256 "e24914aa3383f65a943fe7061e4c542dd6c35143198abcad376458afc72f399e"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.0/trace-darwin-x64"
      sha256 "fd0ce1d02b28c421ae59fc6bdfeb353f96dc5d82301ebbf2e21faac9824bb4dc"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.0/trace-linux-arm64"
      sha256 "d54ff14ffc68b18eed82860d7d4588f56b02aa2960f3f2dade33c32ab647d71d"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.0/trace-linux-x64"
      sha256 "7141225f7c1c06c4fbd077fb5ec38ddd4cf3d98eb4a9d5c1d4144f3365e112ba"
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
