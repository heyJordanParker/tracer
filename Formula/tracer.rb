class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.8.3"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.3/trace-darwin-arm64"
      sha256 "f94661405c098e3270e51979d96dd93d8e7c8d8935e82afbf0ec2d48e8840a9e"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.3/trace-darwin-x64"
      sha256 "df77f4db4e3775ec5dfad03c949705de68692657b054c4d2455dcf22e9358605"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.3/trace-linux-arm64"
      sha256 "ce1b609515e5a3905a8317cb8e1a80627bd5a3476ab7127e38353492149a23ed"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.3/trace-linux-x64"
      sha256 "edf11aba1827ad9f1865a9a0867375e7deaba308b0ae39be8e5d81ce5b0ca9b7"
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
