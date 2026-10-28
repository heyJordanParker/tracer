class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.6.2"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.2/trace-darwin-arm64"
      sha256 "da859e671b0bccde0e02e2fb4085cbd8960fb675e47d6b184d776b5363e11101"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.2/trace-darwin-x64"
      sha256 "46549448044f119997fbb111ac8b0c3905e7d8b0a15cac3220800dfcb3092373"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.2/trace-linux-arm64"
      sha256 "c45dd2386587aa696bb4ddd7cfc1ba04206f211f8c18f81eab013cd5f774d62f"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.6.2/trace-linux-x64"
      sha256 "2567d77c487f254f65c913e71ef0e4b0aaa1d577efed76aecbcd394bebd91566"
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
