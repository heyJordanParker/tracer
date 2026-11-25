class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.10.0"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.10.0/trace-darwin-arm64"
      sha256 "3a91b270023df9947be78026f60e63a0469b07ca81c5aa3dd77f2b3b824a7af0"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.10.0/trace-darwin-x64"
      sha256 "8f4b51f116f2089ce5bb357959d7473244cddec9b256ad0d00beaf26db39231d"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.10.0/trace-linux-arm64"
      sha256 "dd297e8e59e401cc7d7b58c0a41b8845ab5cbda424ef9b0629a0699e142523a9"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.10.0/trace-linux-x64"
      sha256 "2a0163c26f11c229c03b21e88889ff98c121d9e540a152345cbab6f7edfbf1b6"
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
