class Shac < Formula
  desc "Local shell autocomplete engine for zsh and bash"
  homepage "https://github.com/Neftedollar/sh-autocomplete"
  url "https://github.com/Neftedollar/sh-autocomplete/archive/refs/tags/v0.1.3.tar.gz"
  sha256 "fee8db0fdd4a171a6cc352d5f19e2259503cd61d7427caf554f34dabbc1c899a"
  license "MIT"
  head "https://github.com/Neftedollar/sh-autocomplete.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args(path: ".")
    pkgshare.install "shell"
  end

  def caveats
    <<~EOS
      Install shell integration with:
        shac install --shell zsh --edit-rc

      Start and inspect the daemon with:
        shac daemon start
        shac doctor
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/shac --version")
    assert_match "stopped", shell_output("#{bin}/shac daemon status")
  end
end
