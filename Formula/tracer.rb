class Tracer < Formula
  desc "Code intelligence for coding agents"
  homepage "https://github.com/heyJordanParker/tracer"
  version "0.7.0"
  license "MIT"

  depends_on "ast-grep"
  depends_on "ripgrep"
  depends_on "scc"
  depends_on "universal-ctags"

  on_macos do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.7.0/trace-darwin-arm64"
      sha256 "367e3a4097b7d09476747e580b672bf5969ddf425fccfc8b78a3306dadb3aaa7"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.7.0/trace-darwin-x64"
      sha256 "c9e0c5959f1c0f541abccd41c87121b0968e7511e5f93642f7641f407bf80268"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.7.0/trace-linux-arm64"
      sha256 "f2b20289c5c55e6865e51f8632452d44249ddf6301f3147c06d43b402de26a66"
    end
    on_intel do
      url "https://github.com/heyJordanParker/tracer/releases/download/v0.7.0/trace-linux-x64"
      sha256 "5a71d61fd7b3d1798e2edde49846ecd1fac0ff93d09aadcf6bd6878df05ba9bc"
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
