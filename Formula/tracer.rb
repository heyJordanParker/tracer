class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.1.0"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.1.0/trace-darwin-arm64"
      sha256 "64ff7330cb81323ed750f642c6212f438ee725c2d057fa9c45b33e17fac03f93"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.1.0/trace-darwin-x64"
      sha256 "0bfebede22a478866f9b034483380f81e40025ef19eaf86fd49d440ac336f34f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.1.0/trace-linux-arm64"
      sha256 "6a49b73b440ae49f026c4bc89fbbf350003bf953b3f8a35f72c12390da9b8f81"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.1.0/trace-linux-x64"
      sha256 "6b5b3223615b6d0de6fe37097469ae4fc8a3ff6d5369e26c18d42ead9013d528"
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
