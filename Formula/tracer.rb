class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.6.3"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.3/trace-darwin-arm64"
      sha256 "7999e71182ea79be2292574e2a7fd90259921e06e0c82827fa3b7b91d59d42cd"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.3/trace-darwin-x64"
      sha256 "351406214633a1ad944098cdc22e5f5f4a59e35bf995b463008cf42e0b1c0935"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.3/trace-linux-arm64"
      sha256 "b7159ff67ca3119bf66e0d2083e4474e86e4c18cd65660ab71da3d5a880e2cab"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.3/trace-linux-x64"
      sha256 "55006b6c815ada10663940334c3f8b65ac17c194b5be535518799bb3405ce0ea"
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
