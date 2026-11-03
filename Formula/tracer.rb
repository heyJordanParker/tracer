class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.8.2"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.2/trace-darwin-arm64"
      sha256 "e65455b42b06ef3ded0431061033fec24c1171425cdd5e79ef715fb39652b64d"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.2/trace-darwin-x64"
      sha256 "c1f61db22806f74cc1017f8b292198ef0215a48ce354d514d26bb5369b194351"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.2/trace-linux-arm64"
      sha256 "4dfd55d92d96f2d47a1829e7d77c2ff5df09165c6895958ea8c31050d244afae"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.8.2/trace-linux-x64"
      sha256 "6e03f217f35d150489cc73ed700b4609367f93e6ed822cfd158d4b46ef9668e9"
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
