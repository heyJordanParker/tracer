class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.4.0"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.4.0/trace-darwin-arm64"
      sha256 "6b13065cb5094dcc56de9914b868b45e0a5c832703d6112ed4e2d93f5ae2100f"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.4.0/trace-darwin-x64"
      sha256 "34eb2cb8b63ca5a2d7a0d510c5d548d8c17495acf16f3a8b27c525e138977c1c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.4.0/trace-linux-arm64"
      sha256 "dadbc7b74a3270da465e788e0708aebb37ec42080c06dd367f588b2a9a32b93b"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.4.0/trace-linux-x64"
      sha256 "17f9a8db0d5eceaa0626f2b7544cdd708e361c69787ccdeb19753b115626161d"
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
