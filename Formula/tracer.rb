class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.5.1"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.5.1/trace-darwin-arm64"
      sha256 "90c8eab17f79975ecd1e0977d9c09549d063796615f353b959d25f79cac24e53"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.5.1/trace-darwin-x64"
      sha256 "bb26d8c904aa0fa03d8f3b0c4d2bd3bd8cbd0f91777843fc79cbea8f1abfd80e"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.5.1/trace-linux-arm64"
      sha256 "05888a90a9d5750cc9b58adb37d08abbe239498dda8f7fb2332b1023f4d14e9f"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.5.1/trace-linux-x64"
      sha256 "57c33cbc17393847bf1579c194953ee0163b20117d37054b73c7bd498e3316ef"
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
