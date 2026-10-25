class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.6.0"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.0/trace-darwin-arm64"
      sha256 "ab0c95b90b05dde91c0be23e942a8df2ec300276c65f071e1aa3588a01424de2"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.0/trace-darwin-x64"
      sha256 "817389b99a0da08e23fa02f578c941b7dff9c426392739995403f78cf730ee0e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.0/trace-linux-arm64"
      sha256 "3bbb3cfa8f1afd1bbc786e78d18d317e29100292e652803c0bb124946bf3bd9a"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.0/trace-linux-x64"
      sha256 "ef20052c0e0210bd17bdec06f56f525f685182cae7bdefa36c5d25d253a623d9"
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
